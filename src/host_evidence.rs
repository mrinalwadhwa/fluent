//! Immutable, host-produced evidence attached to a reviewed Attempt.

use anyhow::{Context, Result, bail};
use chrono::DateTime;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::path::{Component, Path};

use crate::review::{self, Verdict};
use crate::work_model::{
    self, ArtifactRef, EvidenceAttachment, EvidenceRecovery, EvidenceRecoveryState,
    EvidenceReviewContext, EvidenceReviewTarget, Task, TaskKind, TaskStatus, WorkModelError,
    WorkModelStore,
};

const MAX_EVIDENCE_BYTES: usize = 1024 * 1024;

/// Version-one operator evidence document. Its serialized bytes, not a
/// re-rendering, are the audited artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceDocument {
    pub schema_version: u32,
    pub producer: String,
    pub check: String,
    pub working_directory: String,
    pub result: EvidenceResult,
    pub run_at: String,
    pub output: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EvidenceResult {
    Pass,
    Fail,
}

pub fn attach(
    project_root: &Path,
    store: &WorkModelStore,
    work_item_id: &str,
    attempt_id: &str,
    candidate_commit: &str,
    evidence_file: &Path,
    review_artifacts: &[String],
) -> Result<EvidenceRecovery> {
    if fs::symlink_metadata(evidence_file)
        .with_context(|| format!("inspect evidence file {}", evidence_file.display()))?
        .file_type()
        .is_symlink()
    {
        bail!("evidence file must not be a symlink");
    }
    let bytes = fs::read(evidence_file)
        .with_context(|| format!("read evidence file {}", evidence_file.display()))?;
    let document = parse_document(&bytes)?;
    let digest = format!("sha256:{:x}", Sha256::digest(&bytes));
    let snapshot_path = snapshot_path(work_item_id, attempt_id, &digest)?;

    let recovery = store.mutate_work_item(work_item_id, |item| {
        let existing = item
            .attempts
            .iter()
            .find(|a| a.id == attempt_id)
            .ok_or_else(|| WorkModelError::AttemptNotFound {
                id: attempt_id.to_string(),
            })?;
        if let Some(existing) = existing.evidence_recoveries.iter().find(|existing| {
            existing.candidate_commit == candidate_commit
                && existing.attachment.digest == digest
                && existing.attachment.snapshot_path == snapshot_path
        }) {
            return Ok(existing.clone());
        }
        let prior_reviews = validate_evidence_frontier(
            project_root,
            item,
            attempt_id,
            candidate_commit,
            review_artifacts,
        )?;
        // Publish only after the lock-held frontier check. A rejected request
        // therefore cannot leave a host-owned artifact behind.
        publish_snapshot(project_root, &snapshot_path, &bytes).map_err(|_| {
            WorkModelError::AttemptNotFound {
                id: attempt_id.to_string(),
            }
        })?;
        let attempt = item
            .attempts
            .iter_mut()
            .find(|a| a.id == attempt_id)
            .expect("validated Attempt must remain present");
        let roles = prior_reviews
            .iter()
            .map(|task| task.role.clone())
            .collect::<Vec<_>>();
        let recovery = attempt.attach_evidence_recovery(EvidenceRecovery {
            id: format!("host-evidence-{}", &digest[7..19]),
            candidate_commit: candidate_commit.to_string(),
            attachment: EvidenceAttachment {
                snapshot_path: snapshot_path.clone(),
                digest: digest.clone(),
            },
            targets: roles
                .iter()
                .map(|role| EvidenceReviewTarget {
                    role: role.clone(),
                    prior_review_artifact: review_artifacts
                        .iter()
                        .find(|path| {
                            prior_reviews.iter().any(|task| task.role == *role
                                && review_artifact_path(task).as_deref() == Some(path.as_str()))
                        })
                        .cloned()
                        .unwrap_or_default(),
                    review_task_id: None,
                })
                .collect(),
            state: EvidenceRecoveryState::Reviewing,
            created_at: work_model::now_iso8601(),
        })?;
        for role in roles {
            let prior = prior_reviews
                .iter()
                .find(|task| task.role == role)
                .cloned()
                .unwrap();
            let task_id = format!("{attempt_id}-evidence-{}-{role}", &digest[7..19]);
            if !attempt.tasks.iter().any(|task| task.id == task_id) {
                let mut inputs = prior.input_artifacts.clone();
                inputs.push(ArtifactRef {
                    producer_id: "host-evidence".to_string(),
                    path: snapshot_path.clone(),
                });
                inputs.push(ArtifactRef {
                    producer_id: prior.id.clone(),
                    path: review_artifact_path(&prior).unwrap(),
                });
                let prior_review_artifact = review_artifact_path(&prior).unwrap();
                attempt.tasks.push(Task {
                    id: task_id.clone(),
                    kind: TaskKind::Review,
                    status: TaskStatus::Planned,
                    role: role.clone(),
                    instructions: None,
                    work_item_id: work_item_id.to_string(),
                    attempt_id: Some(attempt_id.to_string()),
                    workspace_access: prior.workspace_access,
                    artifact_area: Some(work_model::TaskArtifactArea {
                        path: work_model::work_artifact_path(work_item_id, attempt_id, &task_id),
                    }),
                    review_context: prior.review_context.clone(),
                    evidence_review_context: Some(EvidenceReviewContext {
                        recovery_id: recovery.id.clone(),
                        candidate_commit: candidate_commit.to_string(),
                        attachment: recovery.attachment.clone(),
                        prior_review_artifact,
                    }),
                    input_artifacts: inputs,
                    depends_on: None,
                    output: None,
                    created_at: Some(work_model::now_iso8601()),
                    started_at: None,
                    completed_at: None,
                });
            }
            if let Some(target) = attempt
                .evidence_recoveries
                .iter_mut()
                .find(|entry| entry.id == recovery.id)
                .and_then(|entry| entry.targets.iter_mut().find(|target| target.role == role))
            {
                target.review_task_id = Some(task_id);
            }
        }
        attempt.status = work_model::AttemptStatus::Reviewing;
        attempt.review_state = Some(work_model::AttemptReviewState::NotReviewed);
        Ok(recovery)
    })?;
    let _ = document; // validation intentionally happens before publication.
    Ok(recovery)
}

fn validate_evidence_frontier(
    project_root: &Path,
    item: &work_model::WorkItem,
    attempt_id: &str,
    candidate_commit: &str,
    review_artifacts: &[String],
) -> Result<Vec<Task>, WorkModelError> {
    let rejected = || WorkModelError::AttemptNotFound { id: attempt_id.to_string() };
    let attempt = item.attempts.iter().find(|attempt| attempt.id == attempt_id).ok_or_else(rejected)?;
    if review_artifacts.is_empty()
        || review_artifacts.len() != review_artifacts.iter().collect::<std::collections::HashSet<_>>().len()
        || attempt.tasks.iter().any(|task| task.status == TaskStatus::Executing)
        || item.merge_candidates.iter().any(|candidate| candidate.attempt_id == attempt_id
            && candidate.candidate_commit == candidate_commit)
    {
        return Err(rejected());
    }
    let completed_writer = attempt.tasks.iter().enumerate().rev().find(|(_, task)| {
        task.kind == TaskKind::Write && task.status == TaskStatus::Complete
    }).and_then(|(index, task)| task.output.as_ref().map(|output| (index, output))).ok_or_else(rejected)?;
    if completed_writer.1.commit != candidate_commit
        || !candidate_workspace_is_clean_at(project_root, completed_writer.1, candidate_commit)
    {
        return Err(rejected());
    }
    let last_writer = attempt.tasks.iter().enumerate().rev().find(|(_, task)| task.kind == TaskKind::Write);
    let is_legacy = last_writer.is_some_and(|(index, task)| {
        task.status == TaskStatus::Failed
            && task.output.is_none()
            && index > completed_writer.0
            && index + 1 == attempt.tasks.len()
            && task.input_artifacts.iter().map(|input| &input.path).collect::<std::collections::HashSet<_>>()
                == review_artifacts.iter().collect()
    });
    let is_paused_evidence_recovery = attempt.status == work_model::AttemptStatus::NeedsUser
        && attempt.pause_kind == Some(work_model::PauseKind::Uncertain)
        && attempt.evidence_recoveries.last().is_some_and(|recovery| {
            recovery.state == EvidenceRecoveryState::NeedsEvidence
                && recovery.candidate_commit == candidate_commit
                && attempt.tasks.iter().any(|task| {
                    task.evidence_review_context.as_ref().is_some_and(|context| {
                        context.recovery_id == recovery.id
                            && recovery.targets.iter().any(|target| target.role == task.role)
                    })
                })
        });
    if !(attempt.status == work_model::AttemptStatus::Reviewing
        || is_legacy
        || is_paused_evidence_recovery)
    {
        return Err(rejected());
    }
    let mut selected = Vec::new();
    for path in review_artifacts {
        let (index, task) = attempt.tasks.iter().enumerate().find(|(_, task)| {
            review_artifact_path(task).as_deref() == Some(path.as_str())
        }).ok_or_else(rejected)?;
        if task.kind != TaskKind::Review
            || task.status != TaskStatus::Complete
            || task.review_context.as_ref().map(|context| context.candidate_commit.as_str()) != Some(candidate_commit)
            || !matches!(read_verdict(project_root, task), Some(Verdict::Fail))
            || index <= completed_writer.0
            || attempt.tasks.iter().skip(index + 1).any(|later| {
                later.kind == TaskKind::Review && later.role == task.role && later.status == TaskStatus::Complete
            })
            || selected.iter().any(|prior: &Task| prior.role == task.role)
            || (is_paused_evidence_recovery
                && !task.evidence_review_context.as_ref().is_some_and(|context| {
                    attempt.evidence_recoveries.last().is_some_and(|recovery| {
                        context.recovery_id == recovery.id
                            && recovery.state == EvidenceRecoveryState::NeedsEvidence
                    })
                }))
        {
            return Err(rejected());
        }
        selected.push(task.clone());
    }
    Ok(selected)
}

fn candidate_workspace_is_clean_at(
    project_root: &Path,
    output: &crate::work_model::TaskOutput,
    candidate: &str,
) -> bool {
    let workspace = Path::new(&output.workspace_path);
    let workspace = if workspace.is_absolute() {
        workspace.to_path_buf()
    } else {
        project_root.join(workspace)
    };
    crate::git::run_stdout(
        &workspace,
        &["rev-parse", "HEAD"],
        "inspect evidence candidate",
    )
    .is_ok_and(|head| head == candidate)
        && crate::git::run_stdout(
            &workspace,
            &["status", "--porcelain", "--untracked-files=all"],
            "inspect evidence candidate cleanliness",
        )
        .is_ok_and(|status| status.is_empty())
}

fn parse_document(bytes: &[u8]) -> Result<EvidenceDocument> {
    if bytes.is_empty() || bytes.len() > MAX_EVIDENCE_BYTES {
        bail!("evidence document must be between 1 and {MAX_EVIDENCE_BYTES} bytes");
    }
    let document: EvidenceDocument = serde_json::from_slice(bytes)
        .context("evidence document must be valid schema-version-1 JSON")?;
    if document.schema_version != 1
        || document.producer.trim().is_empty()
        || document.check.trim().is_empty()
        || document.working_directory.trim().is_empty()
    {
        bail!("evidence document has an unsupported version or empty required field");
    }
    DateTime::parse_from_rfc3339(&document.run_at).context("evidence run_at must be RFC3339")?;
    Ok(document)
}

fn snapshot_path(work: &str, attempt: &str, digest: &str) -> Result<String> {
    let hex = digest
        .strip_prefix("sha256:")
        .context("invalid evidence digest")?;
    Ok(format!(
        ".fluent/work/artifacts/{work}/{attempt}/host-evidence/{hex}.json"
    ))
}
fn review_artifact_path(task: &Task) -> Option<String> {
    task.artifact_area
        .as_ref()
        .map(|area| format!("{}/review.md", area.path))
}
fn read_verdict(project_root: &Path, task: &Task) -> Option<Verdict> {
    fs::read_to_string(project_root.join(review_artifact_path(task)?))
        .ok()
        .map(|text| review::extract_verdict(&text))
}

fn publish_snapshot(project_root: &Path, relative: &str, bytes: &[u8]) -> Result<()> {
    let relative_path = Path::new(relative);
    let managed_root = Path::new(".fluent/work/artifacts");
    if relative_path.is_absolute() || !relative_path.starts_with(managed_root) {
        bail!("evidence snapshot path must stay under the managed artifact root");
    }
    let mut current = project_root.to_path_buf();
    for component in relative_path.components() {
        let Component::Normal(part) = component else {
            bail!("evidence snapshot path must use normal managed components");
        };
        current.push(part);
        if current == project_root.join(relative_path) {
            break;
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                bail!("evidence snapshot path must not traverse a symlink")
            }
            Ok(metadata) if !metadata.is_dir() => {
                bail!("evidence snapshot parent must be a directory")
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&current).with_context(|| {
                    format!("create managed evidence directory {}", current.display())
                })?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    let path = project_root.join(relative_path);
    match tempfile::NamedTempFile::new_in(path.parent().expect("snapshot has a parent")) {
        Ok(mut file) => {
            file.write_all(bytes)?;
            file.as_file().sync_all()?;
            match file.persist_noclobber(&path) {
                Ok(_) => {}
                Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if fs::symlink_metadata(&path)?.file_type().is_symlink() {
                        bail!("evidence snapshot path must not be a symlink");
                    }
                    if fs::read(&path)? != bytes {
                        bail!("evidence snapshot digest collision at {}", path.display());
                    }
                }
                Err(error) => return Err(error.error.into()),
            }
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evidence_document_requires_versioned_complete_host_claim() {
        let accepted = br#"{"schema_version":1,"producer":"host","check":"fluent tester check","working_directory":"/repo","result":"pass","run_at":"2026-08-03T17:59:47Z","output":"ok"}"#;
        assert_eq!(
            parse_document(accepted).unwrap().result,
            EvidenceResult::Pass
        );

        let missing_check = br#"{"schema_version":1,"producer":"host","check":"","working_directory":"/repo","result":"pass","run_at":"2026-08-03T17:59:47Z","output":"ok"}"#;
        assert!(parse_document(missing_check).is_err());
    }

    #[test]
    fn evidence_snapshot_is_exact_and_idempotent() {
        let root = tempfile::TempDir::new().unwrap();
        let bytes = b"exact host output\n";
        publish_snapshot(
            root.path(),
            ".fluent/work/artifacts/work/a/host-evidence/digest.json",
            bytes,
        )
        .unwrap();
        publish_snapshot(
            root.path(),
            ".fluent/work/artifacts/work/a/host-evidence/digest.json",
            bytes,
        )
        .unwrap();
        assert_eq!(
            fs::read(
                root.path()
                    .join(".fluent/work/artifacts/work/a/host-evidence/digest.json")
            )
            .unwrap(),
            bytes
        );
        assert!(
            publish_snapshot(
                root.path(),
                ".fluent/work/artifacts/work/a/host-evidence/digest.json",
                b"different"
            )
            .is_err()
        );
    }

    #[test]
    fn stale_evidence_frontier_is_rejected_without_mutation() {
        let root = tempfile::TempDir::new().unwrap();
        let mut item = work_model::WorkItem::planned("work-1", "Attach host evidence");
        item.add_initial_attempt("attempt-1").unwrap();
        let writer = &mut item.attempts[0].tasks[0];
        writer.status = TaskStatus::Complete;
        writer.output = Some(work_model::TaskOutput {
            workspace_id: "candidate".to_string(),
            workspace_path: root.path().display().to_string(),
            source_branch: "main".to_string(),
            base_commit: None,
            commit: "old-candidate".to_string(),
            no_change: None,
            learner_canonicalization: None,
        });

        let before = item.clone();
        assert!(validate_evidence_frontier(
            root.path(),
            &item,
            "attempt-1",
            "new-candidate",
            &["review.md".to_string()],
        )
        .is_err());
        assert_eq!(item, before);
    }

    #[cfg(unix)]
    #[test]
    fn evidence_snapshot_rejects_symlinked_managed_ancestor() {
        use std::os::unix::fs::symlink;

        let root = tempfile::TempDir::new().unwrap();
        let outside = tempfile::TempDir::new().unwrap();
        fs::create_dir_all(root.path().join(".fluent/work")).unwrap();
        symlink(outside.path(), root.path().join(".fluent/work/artifacts")).unwrap();

        assert!(
            publish_snapshot(
                root.path(),
                ".fluent/work/artifacts/work/a/host-evidence/digest.json",
                b"exact host output\n",
            )
            .is_err()
        );
        assert!(fs::read_dir(outside.path()).unwrap().next().is_none());
    }
}
