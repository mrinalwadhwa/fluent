//! Versioned, type-only contracts for the learning follow-up flywheel.
//!
//! These structures pin the serialized shape of a learner handoff and the
//! follow-up drafts and learning record it carries. Producing and consuming
//! handoffs lands in later Work Items; this module fixes the vocabulary and the
//! on-disk format now so the scheduler and Learner branches cannot diverge.

use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::work_model::CorrectiveContext;

/// Content-addressed pointer to a file within a learner handoff. The path is
/// always relative to the handoff root; the digest lets a consumer verify the
/// referenced content before materializing it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRef {
    /// Path relative to the handoff root. Never absolute, so a handoff stays
    /// portable across machines and checkouts.
    pub path: String,
    /// Digest of the referenced content, e.g. `sha256:...`.
    pub digest: String,
}

impl ArtifactRef {
    /// Whether the reference is relative, as a portable handoff requires.
    pub fn is_relative(&self) -> bool {
        !Path::new(&self.path).is_absolute()
    }
}

/// A durable learning captured by a Learner after a code-producing Attempt
/// passes its reviews: refined project expertise summarized alongside the files
/// it touched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct LearningRecord {
    /// Human-readable summary of what was learned.
    pub summary: String,
    /// Expertise files the Learner refined, relative and digest-bearing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expertise: Vec<ArtifactRef>,
}

/// One independently identifiable follow-up described by a Learner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct FollowUpDraftV1 {
    /// Stable identifier of this follow-up within its handoff.
    pub id: String,
    /// One-line description of the proposed follow-up.
    pub summary: String,
    /// Whether this follow-up is corrective — eligible to bypass the usual
    /// brief, behaviors, approach, and plan conversation.
    #[serde(default)]
    pub corrective: bool,
    /// The complete corrective execution input, present when `corrective`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corrective_context: Option<CorrectiveContext>,
    /// Supporting evidence, relative and digest-bearing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<ArtifactRef>,
}

fn schema_version_v1() -> u32 {
    LearnerHandoffV1::SCHEMA_VERSION
}

/// A portable learner handoff: the operational output of learning capture,
/// carrying zero or more independently identifiable follow-ups and the learning
/// record. It is not itself an Observation or a Work Item; materializing it into
/// a project's local Observation backlog lands in a later Work Item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LearnerHandoffV1 {
    /// Schema version, pinned to 1 for this contract so future readers can
    /// detect the format even from an older handoff.
    #[serde(default = "schema_version_v1")]
    pub schema_version: u32,
    /// The Work Item whose code-producing Attempt produced this handoff.
    pub source_work_item_id: String,
    /// The Attempt whose accepted Write result triggered learning capture.
    pub source_attempt_id: String,
    /// The Merge Candidate this handoff was produced alongside. Stamped by the
    /// host from the resulting candidate identity, not the untrusted draft.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_merge_candidate_id: Option<String>,
    /// The learning captured from that Attempt.
    pub learning: LearningRecord,
    /// Zero or more independently identifiable follow-ups.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub follow_ups: Vec<FollowUpDraftV1>,
}

impl LearnerHandoffV1 {
    /// The pinned schema version for this handoff contract.
    pub const SCHEMA_VERSION: u32 = 1;

    /// A handoff with no follow-ups yet — the baseline for learning capture that
    /// only refined expertise.
    pub fn new(
        source_work_item_id: impl Into<String>,
        source_attempt_id: impl Into<String>,
        learning: LearningRecord,
    ) -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION,
            source_work_item_id: source_work_item_id.into(),
            source_attempt_id: source_attempt_id.into(),
            source_merge_candidate_id: None,
            learning,
            follow_ups: Vec::new(),
        }
    }
}

/// The untrusted draft a Learner writes into its managed handoff surface. The
/// host validates it, stamps authoritative provenance, and canonicalizes it into
/// an immutable [`LearnerHandoffV1`]. The draft carries only what the Learner is
/// trusted to describe — a learning summary and candidate follow-ups — never the
/// source identities, which the host supplies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct LearnerDraftV1 {
    /// Human-readable summary of what was learned.
    #[serde(default)]
    pub learning_summary: String,
    /// Zero or more independently identifiable follow-ups the Learner proposes.
    #[serde(default)]
    pub follow_ups: Vec<FollowUpDraftV1>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_handoff() -> LearnerHandoffV1 {
        LearnerHandoffV1 {
            schema_version: LearnerHandoffV1::SCHEMA_VERSION,
            source_work_item_id: "work-1".to_string(),
            source_attempt_id: "attempt-1".to_string(),
            source_merge_candidate_id: Some("attempt-1-merge-candidate".to_string()),
            learning: LearningRecord {
                summary: "Cap enforcement belongs in retry.rs".to_string(),
                expertise: vec![ArtifactRef {
                    path: "expertise/retry.md".to_string(),
                    digest: "sha256:abc".to_string(),
                }],
            },
            follow_ups: vec![FollowUpDraftV1 {
                id: "fu-1".to_string(),
                summary: "Restore the retry cap".to_string(),
                corrective: true,
                corrective_context: Some(CorrectiveContext {
                    objective: "Restore the retry guard".to_string(),
                    requirement: "Retries stop after the configured cap".to_string(),
                    evidence: "Merged commit abc123 removed the cap check".to_string(),
                    included_scope: "src/retry.rs".to_string(),
                    excluded_scope: "unrelated backoff tuning".to_string(),
                    verification: "cargo test retry::cap_is_enforced".to_string(),
                }),
                evidence: vec![ArtifactRef {
                    path: "evidence/diff.patch".to_string(),
                    digest: "sha256:def".to_string(),
                }],
            }],
        }
    }

    #[test]
    fn learner_handoff_round_trips_through_json() {
        let handoff = sample_handoff();
        let json = serde_json::to_string_pretty(&handoff).unwrap();
        let restored: LearnerHandoffV1 = serde_json::from_str(&json).unwrap();
        assert_eq!(handoff, restored);
    }

    #[test]
    fn handoff_without_schema_version_defaults_to_v1() {
        // A handoff serialized before the version marker existed reads as v1
        // rather than failing to deserialize.
        let json = r#"{
            "source_work_item_id": "work-1",
            "source_attempt_id": "attempt-1",
            "learning": { "summary": "learned" }
        }"#;
        let handoff: LearnerHandoffV1 = serde_json::from_str(json).unwrap();
        assert_eq!(handoff.schema_version, LearnerHandoffV1::SCHEMA_VERSION);
        assert!(handoff.follow_ups.is_empty());
    }

    #[test]
    fn new_handoff_carries_no_follow_ups() {
        let handoff = LearnerHandoffV1::new(
            "work-1",
            "attempt-1",
            LearningRecord {
                summary: "learned".to_string(),
                expertise: Vec::new(),
            },
        );
        assert_eq!(handoff.schema_version, 1);
        assert!(handoff.follow_ups.is_empty());
    }

    #[test]
    fn artifact_ref_reports_relative_paths() {
        let relative = ArtifactRef {
            path: "evidence/diff.patch".to_string(),
            digest: "sha256:def".to_string(),
        };
        let absolute = ArtifactRef {
            path: "/tmp/diff.patch".to_string(),
            digest: "sha256:def".to_string(),
        };
        assert!(relative.is_relative());
        assert!(!absolute.is_relative());
    }
}
