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
    format!(
        "{}/{DRAFT_FILE_NAME}",
        handoff_dir_rel(work_item_id, attempt_id)
    )
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
pub fn read_draft(
    project_root: &Path,
    work_item_id: &str,
    attempt_id: &str,
) -> Result<LearnerDraftV1> {
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
    let (draft, _normalizations) = normalize_draft(draft);
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

/// Normalize a Learner draft toward a safely persistable, Observation-only
/// state. It only ever moves a follow-up *away* from corrective authority, never
/// toward it: malformed optional artifact evidence is dropped, and malformed,
/// incomplete, or unsupported corrective metadata downgrades the follow-up to a
/// plain Observation. Returns the normalized draft plus a note for every change,
/// so one malformed optional field cannot reject the whole draft yet no
/// normalization passes silently.
pub fn normalize_draft(mut draft: LearnerDraftV1) -> (LearnerDraftV1, Vec<String>) {
    let mut notes = Vec::new();
    for follow_up in &mut draft.follow_ups {
        normalize_follow_up(follow_up, &mut notes);
    }
    (draft, notes)
}

fn normalize_follow_up(follow_up: &mut FollowUpDraftV1, notes: &mut Vec<String>) {
    // Drop malformed optional artifact evidence: a bad digest or absolute path
    // must not sink an otherwise-valid Observation.
    let before = follow_up.evidence.len();
    follow_up.evidence.retain(evidence_is_well_formed);
    let dropped = before - follow_up.evidence.len();
    if dropped > 0 {
        notes.push(format!(
            "follow-up {:?}: dropped {dropped} malformed artifact-evidence entr{}, \
             preserved as an Observation",
            follow_up.id,
            if dropped == 1 { "y" } else { "ies" }
        ));
    }
    // Downgrade malformed, incomplete, or unsupported corrective metadata to
    // Observation-only. Never upgrade: a non-corrective follow-up that carries a
    // stray corrective context or authority simply loses it.
    if follow_up.corrective {
        if let Err(reason) = corrective_metadata_reason(follow_up) {
            follow_up.corrective = false;
            follow_up.corrective_context = None;
            follow_up.authority = None;
            notes.push(format!(
                "follow-up {:?}: downgraded to Observation-only ({reason})",
                follow_up.id
            ));
        }
    } else if follow_up.corrective_context.is_some() || follow_up.authority.is_some() {
        follow_up.corrective_context = None;
        follow_up.authority = None;
        notes.push(format!(
            "follow-up {:?}: dropped corrective metadata from a non-corrective follow-up",
            follow_up.id
        ));
    }
}

/// Accept a schema repair only when it preserves the identity and semantic
/// content of every follow-up in the prior draft. A repair may add the schema
/// fields a validation error demanded, but it must not drop a prior follow-up id
/// or rewrite a prior follow-up's non-schema content (its summary, corrective
/// intent, corrective context, target paths, expected result, unresolved
/// decisions, or cited authority). A repair that does is rejected so the earlier
/// draft is retained rather than silently lost or altered.
///
/// A prior follow-up with an empty id cannot be tracked across the repair, so it
/// imposes no preservation constraint; the repair is free to give it an id.
pub fn accept_schema_repair(prior: &LearnerDraftV1, repaired: &LearnerDraftV1) -> Result<()> {
    for prior_fu in &prior.follow_ups {
        if prior_fu.id.trim().is_empty() {
            continue;
        }
        let Some(repaired_fu) = repaired
            .follow_ups
            .iter()
            .find(|candidate| candidate.id == prior_fu.id)
        else {
            bail!(
                "schema repair dropped prior follow-up {:?}",
                prior_fu.id
            );
        };
        if !non_schema_content_matches(prior_fu, repaired_fu) {
            bail!(
                "schema repair rewrote the content of prior follow-up {:?}",
                prior_fu.id
            );
        }
    }
    Ok(())
}

/// Whether two follow-ups carry the same non-schema semantic content. Artifact
/// `evidence` is excluded because a repair may legitimately normalize it toward
/// the required empty array; everything else must be preserved verbatim.
fn non_schema_content_matches(prior: &FollowUpDraftV1, repaired: &FollowUpDraftV1) -> bool {
    prior.summary == repaired.summary
        && prior.corrective == repaired.corrective
        && prior.corrective_context == repaired.corrective_context
        && prior.target_paths == repaired.target_paths
        && prior.expected_result == repaired.expected_result
        && prior.unresolved_decisions == repaired.unresolved_decisions
        && prior.authority == repaired.authority
}

/// `Ok` when a follow-up's corrective metadata is structurally complete enough
/// to stand as a corrective execution input, or the reason it is not. Authority
/// freshness is left to the host's corrective gate; this only rejects metadata
/// that could never be corrective at all.
fn corrective_metadata_reason(follow_up: &FollowUpDraftV1) -> Result<()> {
    let context = follow_up
        .corrective_context
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("missing corrective context"))?;
    context
        .validate()
        .map_err(|error| anyhow::anyhow!("incomplete corrective context: {error}"))?;
    Ok(())
}

/// Whether an artifact-evidence reference is well formed: a non-empty relative
/// path with a `sha256:` digest. A malformed reference is dropped rather than
/// rejecting the follow-up that carries it.
fn evidence_is_well_formed(evidence: &ArtifactRef) -> bool {
    !evidence.path.trim().is_empty()
        && evidence.is_relative()
        && evidence
            .digest
            .strip_prefix("sha256:")
            .is_some_and(|hex| !hex.trim().is_empty())
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
            format!(
                "corrective follow-up {:?} has an incomplete context",
                follow_up.id
            )
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
pub fn load_verified_handoff(
    project_root: &Path,
    reference: &ArtifactRef,
) -> Result<LearnerHandoffV1> {
    if !reference.is_relative() {
        bail!("handoff reference {:?} is not relative", reference.path);
    }
    let path = project_root.join(&reference.path);
    let bytes =
        std::fs::read(&path).with_context(|| format!("read handoff at {}", path.display()))?;
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
                ..Default::default()
            }],
        };
        let handoff = stamp_handoff(
            draft,
            "work-1",
            "attempt-1",
            "attempt-1-merge-candidate",
            Vec::new(),
        )
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
    fn malformed_corrective_followup_downgrades_without_batch_failure() {
        let mut incomplete = corrective_context();
        incomplete.verification = "  ".to_string();
        let draft = LearnerDraftV1 {
            learning_summary: String::new(),
            follow_ups: vec![
                FollowUpDraftV1 {
                    id: "fu-bad".to_string(),
                    summary: "Malformed corrective proposal".to_string(),
                    corrective: true,
                    corrective_context: Some(incomplete),
                    ..Default::default()
                },
                FollowUpDraftV1 {
                    id: "fu-ok".to_string(),
                    summary: "A plain observation".to_string(),
                    corrective: false,
                    ..Default::default()
                },
            ],
        };
        // The whole draft is preserved: the malformed corrective downgrades to
        // Observation-only rather than failing the batch.
        let handoff = stamp_handoff(draft, "work-1", "attempt-1", "mc", Vec::new()).unwrap();
        assert_eq!(handoff.follow_ups.len(), 2);
        let downgraded = &handoff.follow_ups[0];
        assert_eq!(downgraded.id, "fu-bad");
        assert_eq!(downgraded.summary, "Malformed corrective proposal");
        assert!(
            !downgraded.corrective,
            "malformed corrective metadata must not create executable Work"
        );
        assert!(downgraded.corrective_context.is_none());
    }

    #[test]
    fn corrective_followup_without_context_downgrades_to_observation() {
        let draft = LearnerDraftV1 {
            learning_summary: String::new(),
            follow_ups: vec![FollowUpDraftV1 {
                id: "fu-1".to_string(),
                summary: "Restore the retry cap".to_string(),
                corrective: true,
                corrective_context: None,
                evidence: Vec::new(),
                ..Default::default()
            }],
        };
        let handoff = stamp_handoff(draft, "work-1", "attempt-1", "mc", Vec::new()).unwrap();
        assert_eq!(handoff.follow_ups.len(), 1);
        assert!(!handoff.follow_ups[0].corrective);
    }

    #[test]
    fn malformed_noncorrective_artifact_evidence_is_preserved_observation_only() {
        let draft = LearnerDraftV1 {
            learning_summary: String::new(),
            follow_ups: vec![FollowUpDraftV1 {
                id: "fu-1".to_string(),
                summary: "Consider tightening the retry cap".to_string(),
                corrective: false,
                evidence: vec![
                    ArtifactRef {
                        path: "/absolute/eviction.log".to_string(),
                        digest: "sha256:abc".to_string(),
                    },
                    ArtifactRef {
                        path: "notes.md".to_string(),
                        digest: "not-a-digest".to_string(),
                    },
                ],
                ..Default::default()
            }],
        };
        let (normalized, notes) = normalize_draft(draft.clone());
        assert!(
            normalized.follow_ups[0].evidence.is_empty(),
            "malformed optional evidence is dropped"
        );
        assert!(!normalized.follow_ups[0].corrective);
        assert!(!notes.is_empty(), "the normalization is recorded");

        // The finding survives into the handoff as an Observation-only follow-up
        // rather than rejecting the whole draft.
        let handoff = stamp_handoff(draft, "work-1", "attempt-1", "mc", Vec::new()).unwrap();
        assert_eq!(handoff.follow_ups.len(), 1);
        assert_eq!(handoff.follow_ups[0].summary, "Consider tightening the retry cap");
        assert!(handoff.follow_ups[0].evidence.is_empty());
        assert!(!handoff.follow_ups[0].corrective);
    }

    #[test]
    fn schema_repair_cannot_silently_drop_or_rewrite_followups() {
        let prior = LearnerDraftV1 {
            learning_summary: "prior".to_string(),
            follow_ups: vec![
                FollowUpDraftV1 {
                    id: "fu-keep".to_string(),
                    summary: "A well-formed observation".to_string(),
                    corrective: false,
                    ..Default::default()
                },
                FollowUpDraftV1 {
                    // Schema-broken: no id. The repair may give it one.
                    id: String::new(),
                    summary: "Needs an id".to_string(),
                    corrective: false,
                    ..Default::default()
                },
            ],
        };

        // A repair that fixes only the schema-broken follow-up, leaving the
        // well-formed one untouched, is accepted.
        let repaired_ok = LearnerDraftV1 {
            learning_summary: "prior".to_string(),
            follow_ups: vec![
                prior.follow_ups[0].clone(),
                FollowUpDraftV1 {
                    id: "fu-new".to_string(),
                    summary: "Needs an id".to_string(),
                    corrective: false,
                    ..Default::default()
                },
            ],
        };
        assert!(accept_schema_repair(&prior, &repaired_ok).is_ok());

        // A repair that drops the well-formed prior follow-up is rejected.
        let repaired_dropped = LearnerDraftV1 {
            learning_summary: "prior".to_string(),
            follow_ups: vec![FollowUpDraftV1 {
                id: "fu-new".to_string(),
                summary: "Needs an id".to_string(),
                corrective: false,
                ..Default::default()
            }],
        };
        let error = accept_schema_repair(&prior, &repaired_dropped).unwrap_err();
        assert!(error.to_string().contains("dropped prior follow-up"));

        // A repair that rewrites the well-formed prior follow-up's content is
        // rejected.
        let mut rewritten = repaired_ok.clone();
        rewritten.follow_ups[0].summary = "Silently rewritten".to_string();
        let error = accept_schema_repair(&prior, &rewritten).unwrap_err();
        assert!(error.to_string().contains("rewrote the content"));
    }

    #[test]
    fn normalize_keeps_a_complete_corrective_follow_up_and_its_valid_evidence() {
        let draft = LearnerDraftV1 {
            learning_summary: String::new(),
            follow_ups: vec![FollowUpDraftV1 {
                id: "fu-1".to_string(),
                summary: "Restore the retry cap".to_string(),
                corrective: true,
                corrective_context: Some(corrective_context()),
                evidence: vec![ArtifactRef {
                    path: "reviews/review.md".to_string(),
                    digest: "sha256:deadbeef".to_string(),
                }],
                ..Default::default()
            }],
        };
        let (normalized, notes) = normalize_draft(draft);
        assert!(
            notes.is_empty(),
            "a well-formed corrective follow-up is not normalized: {notes:?}"
        );
        assert!(normalized.follow_ups[0].corrective);
        assert_eq!(normalized.follow_ups[0].evidence.len(), 1);
    }

    #[test]
    fn stamp_handoff_rejects_duplicate_follow_up_ids() {
        let follow_up = FollowUpDraftV1 {
            id: "fu-1".to_string(),
            summary: "one".to_string(),
            corrective: false,
            corrective_context: None,
            evidence: Vec::new(),
            ..Default::default()
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
        std::fs::write(
            &path,
            b"{\"schema_version\":1,\"source_work_item_id\":\"tampered\"}",
        )
        .unwrap();
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
