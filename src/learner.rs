//! Produce and verify the portable learner handoff.
//!
//! After a code-producing Attempt passes its reviews, the Learner writes an
//! untrusted [`LearnerDraftV1`] into its managed handoff surface. This module
//! validates that draft, stamps authoritative provenance the Learner is not
//! trusted to supply, canonicalizes the result, computes its digest, and writes
//! exactly one immutable [`LearnerHandoffV1`]. It also loads a handoff back
//! through a relative, digest-bearing [`ArtifactRef`], verifying the content
//! before a consumer can act on it.
//!
//! The handoff is inert: producing it creates no Observation, no derived Work
//! Item, and no Work Queue entry. Consuming it into a project's Observation
//! backlog lands in a later Work Item.

use std::path::Path;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::atomic_write::atomic_write;
use crate::follow_up::{
    ArtifactRef, FollowUpDraftV1, LearnerDraftV1, LearnerHandoffV1, LearningRecord,
};
use crate::work_model::work_artifact_path;

/// File the Learner writes its untrusted draft to, within the handoff surface.
pub const DRAFT_FILE_NAME: &str = "follow-up-draft.json";
/// File the host writes the immutable, canonical handoff to.
pub const HANDOFF_FILE_NAME: &str = "handoff.json";

/// The managed handoff surface for an Attempt, relative to the project root.
/// Both the Learner's draft and the host's immutable handoff live here.
pub fn handoff_dir_rel(work_item_id: &str, attempt_id: &str) -> String {
    work_artifact_path(work_item_id, attempt_id, "learner")
}

/// Relative path of the Learner's untrusted draft.
pub fn draft_path_rel(work_item_id: &str, attempt_id: &str) -> String {
    format!("{}/{DRAFT_FILE_NAME}", handoff_dir_rel(work_item_id, attempt_id))
}

/// Relative path of the immutable handoff.
pub fn handoff_path_rel(work_item_id: &str, attempt_id: &str) -> String {
    format!(
        "{}/{HANDOFF_FILE_NAME}",
        handoff_dir_rel(work_item_id, attempt_id)
    )
}

/// Read the Learner's untrusted draft. A missing draft is treated as an empty
/// draft: a Learner that refined only expertise, or nothing, still succeeds with
/// a handoff carrying zero follow-ups.
pub fn read_draft(project_root: &Path, work_item_id: &str, attempt_id: &str) -> Result<LearnerDraftV1> {
    let path = project_root.join(draft_path_rel(work_item_id, attempt_id));
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .with_context(|| format!("parse learner draft at {}", path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(LearnerDraftV1::default()),
        Err(err) => Err(err).with_context(|| format!("read learner draft at {}", path.display())),
    }
}

/// Stamp authoritative provenance onto an untrusted draft, producing a handoff.
///
/// The Learner may describe a learning summary and follow-ups, but never the
/// source identities: those come from the host, which knows the Work Item,
/// Attempt, and resulting Merge Candidate. Corrective follow-ups are validated
/// so an incomplete corrective context cannot slip through as trusted input.
pub fn stamp_handoff(
    draft: LearnerDraftV1,
    source_work_item_id: &str,
    source_attempt_id: &str,
    source_merge_candidate_id: &str,
    expertise: Vec<ArtifactRef>,
) -> Result<LearnerHandoffV1> {
    let mut follow_ups = Vec::with_capacity(draft.follow_ups.len());
    let mut seen = std::collections::HashSet::new();
    for follow_up in draft.follow_ups {
        validate_follow_up(&follow_up)?;
        if !seen.insert(follow_up.id.clone()) {
            bail!("learner draft repeats follow-up id {:?}", follow_up.id);
        }
        follow_ups.push(follow_up);
    }

    Ok(LearnerHandoffV1 {
        schema_version: LearnerHandoffV1::SCHEMA_VERSION,
        source_work_item_id: source_work_item_id.to_string(),
        source_attempt_id: source_attempt_id.to_string(),
        source_merge_candidate_id: Some(source_merge_candidate_id.to_string()),
        learning: LearningRecord {
            summary: draft.learning_summary,
            expertise,
        },
        follow_ups,
    })
}

fn validate_follow_up(follow_up: &FollowUpDraftV1) -> Result<()> {
    if follow_up.id.trim().is_empty() {
        bail!("learner follow-up is missing a stable id");
    }
    if follow_up.summary.trim().is_empty() {
        bail!("learner follow-up {:?} is missing a summary", follow_up.id);
    }
    if follow_up.corrective {
        let context = follow_up.corrective_context.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "corrective follow-up {:?} is missing its corrective context",
                follow_up.id
            )
        })?;
        context.validate().with_context(|| {
            format!("corrective follow-up {:?} has an incomplete context", follow_up.id)
        })?;
    } else if follow_up.corrective_context.is_some() {
        bail!(
            "non-corrective follow-up {:?} must not carry a corrective context",
            follow_up.id
        );
    }
    Ok(())
}

/// The canonical byte encoding of a handoff: sorted-key JSON, so an identical
/// handoff always yields identical bytes and thus an identical digest.
pub fn canonical_bytes(handoff: &LearnerHandoffV1) -> Result<Vec<u8>> {
    // serde_json's default `Value` maps are sorted, so round-tripping through a
    // `Value` produces a stable, field-order-independent encoding.
    let value: serde_json::Value = serde_json::to_value(handoff)?;
    let bytes = serde_json::to_vec(&value)?;
    Ok(bytes)
}

/// The `sha256:` digest of some canonical bytes.
pub fn digest(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("sha256:{:x}", hasher.finalize())
}

/// Atomically write one immutable handoff into the Attempt's managed surface,
/// returning a relative, digest-bearing reference to it. The bytes written to
/// disk are exactly the canonical bytes the digest covers, so a later load can
/// verify the content it reads.
pub fn write_handoff(
    project_root: &Path,
    work_item_id: &str,
    attempt_id: &str,
    handoff: &LearnerHandoffV1,
) -> Result<ArtifactRef> {
    let relative = handoff_path_rel(work_item_id, attempt_id);
    let path = project_root.join(&relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create handoff surface at {}", parent.display()))?;
    }
    let bytes = canonical_bytes(handoff)?;
    let digest = digest(&bytes);
    atomic_write(&path, &bytes).with_context(|| format!("write handoff at {}", path.display()))?;
    Ok(ArtifactRef {
        path: relative,
        digest,
    })
}

/// Load a handoff through its reference, verifying the on-disk content matches
/// the reference digest before returning it. A digest mismatch means the handoff
/// was altered after it was produced and must not be trusted.
pub fn load_verified_handoff(project_root: &Path, reference: &ArtifactRef) -> Result<LearnerHandoffV1> {
    if !reference.is_relative() {
        bail!("handoff reference {:?} is not relative", reference.path);
    }
    let path = project_root.join(&reference.path);
    let bytes = std::fs::read(&path).with_context(|| format!("read handoff at {}", path.display()))?;
    let actual = digest(&bytes);
    if actual != reference.digest {
        bail!(
            "handoff at {} has digest {} but reference expects {}",
            path.display(),
            actual,
            reference.digest
        );
    }
    let handoff = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse handoff at {}", path.display()))?;
    Ok(handoff)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::work_model::CorrectiveContext;

    fn corrective_context() -> CorrectiveContext {
        CorrectiveContext {
            objective: "Restore the retry guard".to_string(),
            requirement: "Retries stop after the configured cap".to_string(),
            evidence: "Merged commit abc123 removed the cap check".to_string(),
            included_scope: "src/retry.rs".to_string(),
            excluded_scope: "unrelated backoff tuning".to_string(),
            verification: "cargo test retry::cap_is_enforced".to_string(),
        }
    }

    #[test]
    fn stamp_handoff_supplies_source_identities_the_draft_cannot() {
        let draft = LearnerDraftV1 {
            learning_summary: "Cap enforcement belongs in retry.rs".to_string(),
            follow_ups: vec![FollowUpDraftV1 {
                id: "fu-1".to_string(),
                summary: "Restore the retry cap".to_string(),
                corrective: true,
                corrective_context: Some(corrective_context()),
                evidence: Vec::new(),
            }],
        };
        let handoff = stamp_handoff(draft, "work-1", "attempt-1", "attempt-1-merge-candidate", Vec::new())
            .unwrap();
        assert_eq!(handoff.source_work_item_id, "work-1");
        assert_eq!(handoff.source_attempt_id, "attempt-1");
        assert_eq!(
            handoff.source_merge_candidate_id.as_deref(),
            Some("attempt-1-merge-candidate")
        );
        assert_eq!(handoff.follow_ups.len(), 1);
    }

    #[test]
    fn stamp_handoff_rejects_incomplete_corrective_follow_up() {
        let mut context = corrective_context();
        context.verification = "  ".to_string();
        let draft = LearnerDraftV1 {
            learning_summary: String::new(),
            follow_ups: vec![FollowUpDraftV1 {
                id: "fu-1".to_string(),
                summary: "Restore the retry cap".to_string(),
                corrective: true,
                corrective_context: Some(context),
                evidence: Vec::new(),
            }],
        };
        assert!(stamp_handoff(draft, "work-1", "attempt-1", "mc", Vec::new()).is_err());
    }

    #[test]
    fn stamp_handoff_rejects_corrective_follow_up_without_context() {
        let draft = LearnerDraftV1 {
            learning_summary: String::new(),
            follow_ups: vec![FollowUpDraftV1 {
                id: "fu-1".to_string(),
                summary: "Restore the retry cap".to_string(),
                corrective: true,
                corrective_context: None,
                evidence: Vec::new(),
            }],
        };
        assert!(stamp_handoff(draft, "work-1", "attempt-1", "mc", Vec::new()).is_err());
    }

    #[test]
    fn stamp_handoff_rejects_duplicate_follow_up_ids() {
        let follow_up = FollowUpDraftV1 {
            id: "fu-1".to_string(),
            summary: "one".to_string(),
            corrective: false,
            corrective_context: None,
            evidence: Vec::new(),
        };
        let draft = LearnerDraftV1 {
            learning_summary: String::new(),
            follow_ups: vec![follow_up.clone(), follow_up],
        };
        assert!(stamp_handoff(draft, "work-1", "attempt-1", "mc", Vec::new()).is_err());
    }

    #[test]
    fn canonical_bytes_are_stable_and_digest_matches() {
        let handoff = LearnerHandoffV1::new(
            "work-1",
            "attempt-1",
            LearningRecord {
                summary: "learned".to_string(),
                expertise: Vec::new(),
            },
        );
        let a = canonical_bytes(&handoff).unwrap();
        let b = canonical_bytes(&handoff).unwrap();
        assert_eq!(a, b);
        assert!(digest(&a).starts_with("sha256:"));
    }

    #[test]
    fn write_then_load_round_trips_and_verifies_digest() {
        let tmp = tempfile::TempDir::new().unwrap();
        let handoff = stamp_handoff(
            LearnerDraftV1 {
                learning_summary: "learned".to_string(),
                follow_ups: Vec::new(),
            },
            "work-1",
            "attempt-1",
            "attempt-1-merge-candidate",
            Vec::new(),
        )
        .unwrap();
        let reference = write_handoff(tmp.path(), "work-1", "attempt-1", &handoff).unwrap();
        assert!(reference.is_relative());
        assert!(reference.path.ends_with("learner/handoff.json"));

        let loaded = load_verified_handoff(tmp.path(), &reference).unwrap();
        assert_eq!(loaded, handoff);
    }

    #[test]
    fn load_rejects_tampered_handoff() {
        let tmp = tempfile::TempDir::new().unwrap();
        let handoff = LearnerHandoffV1::new(
            "work-1",
            "attempt-1",
            LearningRecord {
                summary: "learned".to_string(),
                expertise: Vec::new(),
            },
        );
        let reference = write_handoff(tmp.path(), "work-1", "attempt-1", &handoff).unwrap();
        let path = tmp.path().join(&reference.path);
        std::fs::write(&path, b"{\"schema_version\":1,\"source_work_item_id\":\"tampered\"}").unwrap();
        assert!(load_verified_handoff(tmp.path(), &reference).is_err());
    }

    #[test]
    fn read_draft_treats_missing_as_empty() {
        let tmp = tempfile::TempDir::new().unwrap();
        let draft = read_draft(tmp.path(), "work-1", "attempt-1").unwrap();
        assert!(draft.follow_ups.is_empty());
        assert!(draft.learning_summary.is_empty());
    }
}
