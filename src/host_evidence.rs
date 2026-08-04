//! Immutable, host-produced evidence attached to a reviewed Attempt.

use anyhow::{Context, Result, bail};
use chrono::DateTime;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Component, Path};

use crate::review::{self, Verdict};
use crate::work_model::{
    self, ArtifactRef, EvidenceAttachment, EvidenceRecovery, EvidenceRecoveryState,
    EvidenceReviewContext,
    EvidenceReviewTarget, Task, TaskKind, TaskStatus, WorkModelError, WorkModelStore,
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

    // Publish immutable bytes before the model reference. Content addressing makes
    // this safe to retry; an unused snapshot is harmless and cannot affect a
    // candidate or Work Item transition.
    publish_snapshot(project_root, &snapshot_path, &bytes)?;

    let recovery = store.mutate_work_item(work_item_id, |item| {
        let attempt = item.attempts.iter_mut().find(|a| a.id == attempt_id)
            .ok_or_else(|| WorkModelError::AttemptNotFound { id: attempt_id.to_string() })?;
        let recorded_candidate = attempt.tasks.iter().rev()
            .find(|task| task.kind == TaskKind::Write && task.status == TaskStatus::Complete)
            .and_then(|task| task.output.as_ref())
            .map(|output| output.commit.as_str());
        if recorded_candidate != Some(candidate_commit) || attempt.tasks.iter().any(|t| t.status == TaskStatus::Executing) {
            return Err(WorkModelError::AttemptNotFound { id: attempt_id.to_string() });
        }
        let mut roles = Vec::new();
        for path in review_artifacts {
            let task = attempt.tasks.iter().find(|task| review_artifact_path(task).as_deref() == Some(path.as_str()))
                .ok_or_else(|| WorkModelError::AttemptNotFound { id: attempt_id.to_string() })?;
            if task.kind != TaskKind::Review || task.status != TaskStatus::Complete || !matches!(read_verdict(project_root, task), Some(Verdict::Fail)) {
                return Err(WorkModelError::AttemptNotFound { id: attempt_id.to_string() });
            }
            if !roles.contains(&task.role) { roles.push(task.role.clone()); }
        }
        if roles.is_empty() { return Err(WorkModelError::AttemptNotFound { id: attempt_id.to_string() }); }
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
                        .find(|path| attempt.tasks.iter().any(|task| task.role == *role && review_artifact_path(task).as_deref() == Some(path.as_str())))
                        .cloned()
                        .unwrap_or_default(),
                    review_task_id: None,
                })
                .collect(),
            state: EvidenceRecoveryState::Reviewing,
            created_at: work_model::now_iso8601(),
        })?;
        for role in roles {
            let prior = attempt.tasks.iter().find(|task| task.role == role && review_artifact_path(task).as_deref().is_some_and(|path| review_artifacts.iter().any(|artifact| artifact == path))).unwrap().clone();
            let task_id = format!("{attempt_id}-evidence-{}-{role}", &digest[7..19]);
            if !attempt.tasks.iter().any(|task| task.id == task_id) {
                let mut inputs = prior.input_artifacts.clone();
                inputs.push(ArtifactRef { producer_id: "host-evidence".to_string(), path: snapshot_path.clone() });
                inputs.push(ArtifactRef { producer_id: prior.id.clone(), path: review_artifact_path(&prior).unwrap() });
                let prior_review_artifact = review_artifact_path(&prior).unwrap();
                attempt.tasks.push(Task { id: task_id.clone(), kind: TaskKind::Review, status: TaskStatus::Planned, role: role.clone(), instructions: None, work_item_id: work_item_id.to_string(), attempt_id: Some(attempt_id.to_string()), workspace_access: prior.workspace_access, artifact_area: Some(work_model::TaskArtifactArea { path: work_model::work_artifact_path(work_item_id, attempt_id, &task_id) }), review_context: prior.review_context.clone(), evidence_review_context: Some(EvidenceReviewContext { recovery_id: recovery.id.clone(), candidate_commit: candidate_commit.to_string(), attachment: recovery.attachment.clone(), prior_review_artifact }), input_artifacts: inputs, depends_on: None, output: None, created_at: Some(work_model::now_iso8601()), started_at: None, completed_at: None });
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
    if relative_path.is_absolute()
        || relative_path.components().any(|component| matches!(component, Component::ParentDir))
    {
        bail!("evidence snapshot path must stay under the managed artifact root");
    }
    let path = project_root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    match OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(mut file) => {
            file.write_all(bytes)?;
            file.sync_all()?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if fs::symlink_metadata(&path)?.file_type().is_symlink() {
                bail!("evidence snapshot path must not be a symlink");
            }
            if fs::read(&path)? != bytes {
                bail!("evidence snapshot digest collision at {}", path.display());
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
}
