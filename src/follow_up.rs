//! Versioned, type-only contracts for the learning follow-up flywheel.
//!
//! These structures pin the serialized shape of a learner handoff and the
//! follow-up drafts and learning record it carries. Producing and consuming
//! handoffs lands in later Work Items; this module fixes the vocabulary and the
//! on-disk format now so the scheduler and Learner branches cannot diverge.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

use crate::atomic_write::atomic_write;
use crate::observations::{self, LEARNER_FOLLOW_UP_KIND, ObservationFrontmatter};
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

// -------------------------------------------------------------------------
// Land-gated materialization: pending operation, normalized batch, journal
// -------------------------------------------------------------------------

/// The authoritative origin of a landed follow-up batch: the Work Item, Attempt,
/// Merge Candidate, and the commit it landed as. The host stamps it from the
/// resulting merge identity; a producer never supplies it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PostLandOrigin {
    pub work_item_id: String,
    pub attempt_id: String,
    pub merge_candidate_id: String,
    pub merged_commit: String,
}

/// Which producer a normalized follow-up batch came from. Both normalize into the
/// same batch so one consumer materializes either.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FollowUpSource {
    Learner,
    PostMerge,
}

fn batch_schema_version_v1() -> u32 {
    NormalizedFollowUpBatchV1::SCHEMA_VERSION
}

/// A source-neutral, host-stamped batch of landed follow-ups. A learner handoff
/// and a post-merge review both normalize into this shape, so a single consumer
/// materializes either. Producing a post-merge batch and transporting a Fargate
/// batch land in later Work Items; this item exercises the consumer through the
/// same contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizedFollowUpBatchV1 {
    #[serde(default = "batch_schema_version_v1")]
    pub schema_version: u32,
    pub source: FollowUpSource,
    pub origin: PostLandOrigin,
    #[serde(default)]
    pub learning_summary: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub follow_ups: Vec<FollowUpDraftV1>,
}

impl NormalizedFollowUpBatchV1 {
    pub const SCHEMA_VERSION: u32 = 1;

    /// Normalize a verified learner handoff into a source-neutral batch, stamping
    /// the authoritative origin from the host. The handoff's own source
    /// identities must match that origin, or the handoff belongs to a different
    /// Merge Candidate and is rejected.
    pub fn from_learner_handoff(
        handoff: &LearnerHandoffV1,
        origin: PostLandOrigin,
    ) -> Result<Self> {
        if handoff.source_work_item_id != origin.work_item_id
            || handoff.source_attempt_id != origin.attempt_id
            || handoff.source_merge_candidate_id.as_deref()
                != Some(origin.merge_candidate_id.as_str())
        {
            bail!(
                "learner handoff origin (work item {:?}, attempt {:?}, candidate {:?}) \
                 does not match the Merge Candidate being processed (work item {:?}, \
                 attempt {:?}, candidate {:?})",
                handoff.source_work_item_id,
                handoff.source_attempt_id,
                handoff.source_merge_candidate_id,
                origin.work_item_id,
                origin.attempt_id,
                origin.merge_candidate_id,
            );
        }
        Ok(Self {
            schema_version: Self::SCHEMA_VERSION,
            source: FollowUpSource::Learner,
            origin,
            learning_summary: handoff.learning.summary.clone(),
            follow_ups: handoff.follow_ups.clone(),
        })
    }
}

fn op_schema_version_v1() -> u32 {
    PendingPostLandOperationV1::SCHEMA_VERSION
}

/// The versioned, transport-neutral request to materialize a landed follow-up
/// batch. It carries a stable operation identity, the authoritative origin, a
/// digest-bearing reference to the normalized batch, and an optional review
/// request reference. A remote runtime may transport it, but its local policy is
/// resolved and its effects are produced only by the consuming host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingPostLandOperationV1 {
    #[serde(default = "op_schema_version_v1")]
    pub schema_version: u32,
    pub operation_id: String,
    pub origin: PostLandOrigin,
    /// Reference to the persisted normalized batch, relative and digest-bearing.
    pub batch_ref: ArtifactRef,
    /// Optional reference to the review request that motivated this operation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_request: Option<ArtifactRef>,
    /// Digest over the operation's canonical identity, so a transport can detect
    /// tampering before the local host acts on it.
    pub digest: String,
}

impl PendingPostLandOperationV1 {
    pub const SCHEMA_VERSION: u32 = 1;
}

fn journal_schema_version_v1() -> u32 {
    PostLandJournal::SCHEMA_VERSION
}

/// The resumable journal for one post-land operation. Each effect is written
/// before its receipt is appended, so a replay after a crash reconciles
/// deterministic identities rather than duplicating work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostLandJournal {
    #[serde(default = "journal_schema_version_v1")]
    pub schema_version: u32,
    pub operation_id: String,
    #[serde(default)]
    pub follow_ups: Vec<FollowUpReceipt>,
    #[serde(default)]
    pub completed: bool,
}

impl PostLandJournal {
    pub const SCHEMA_VERSION: u32 = 1;

    fn empty(operation_id: &str) -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION,
            operation_id: operation_id.to_string(),
            follow_ups: Vec::new(),
            completed: false,
        }
    }

    fn receipt(&self, follow_up_id: &str) -> Option<&FollowUpReceipt> {
        self.follow_ups
            .iter()
            .find(|r| r.follow_up_id == follow_up_id)
    }
}

/// What one follow-up produced. Step 1 records the materialized Observation;
/// later steps extend this with the resolved policy, derived Work Item, and queue
/// disposition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FollowUpReceipt {
    pub follow_up_id: String,
    /// The deterministic Observation id this follow-up materialized into.
    pub observation_id: String,
}

/// The result of replaying a post-land operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayOutcome {
    pub operation_id: String,
    /// Observations newly created by this replay (0 on an idempotent re-run).
    pub observations_created: usize,
    /// Follow-ups the batch carries.
    pub follow_ups: usize,
}

/// The canonical byte encoding of a serializable value: sorted-key JSON, so an
/// identical value always yields identical bytes and thus an identical digest.
pub fn canonical_json_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let value: serde_json::Value = serde_json::to_value(value)?;
    Ok(serde_json::to_vec(&value)?)
}

/// The `sha256:` digest of some bytes.
pub fn content_digest(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("sha256:{:x}", hasher.finalize())
}

/// The per-origin post-land journal root, relative to the project.
pub fn follow_ups_root(project_root: &Path) -> PathBuf {
    project_root.join(".fluent/work/follow-ups")
}

fn operation_dir(project_root: &Path, operation_id: &str) -> PathBuf {
    follow_ups_root(project_root).join(operation_id)
}

/// The stable operation identity for a landed origin. A Merge Candidate lands
/// once, so the same land always resolves to the same operation.
pub fn operation_id_for(origin: &PostLandOrigin) -> String {
    format!("{}-{}", origin.work_item_id, origin.merge_candidate_id)
}

fn batch_rel_path(operation_id: &str) -> String {
    format!(".fluent/work/follow-ups/{operation_id}/batch.json")
}

fn operation_identity_digest(operation: &PendingPostLandOperationV1) -> Result<String> {
    let mut identity = operation.clone();
    identity.digest = String::new();
    Ok(content_digest(&canonical_json_bytes(&identity)?))
}

/// Record a landed follow-up batch as a durable, replayable pending operation.
///
/// The normalized batch and the operation are persisted before any effect is
/// produced, so a crash between recording and replay leaves a retryable
/// operation rather than losing the landed follow-ups. Recording is idempotent:
/// re-recording the same origin returns the existing operation, and a recorded
/// operation with a conflicting identity is rejected.
pub fn record_post_land_operation(
    project_root: &Path,
    batch: &NormalizedFollowUpBatchV1,
    review_request: Option<ArtifactRef>,
) -> Result<PendingPostLandOperationV1> {
    let operation_id = operation_id_for(&batch.origin);
    let dir = operation_dir(project_root, &operation_id);
    fs::create_dir_all(&dir)
        .with_context(|| format!("create post-land operation dir {}", dir.display()))?;

    let batch_bytes = canonical_json_bytes(batch)?;
    let batch_digest = content_digest(&batch_bytes);
    let batch_rel = batch_rel_path(&operation_id);
    atomic_write(&project_root.join(&batch_rel), &batch_bytes)
        .with_context(|| format!("write normalized batch {batch_rel}"))?;

    let mut operation = PendingPostLandOperationV1 {
        schema_version: PendingPostLandOperationV1::SCHEMA_VERSION,
        operation_id: operation_id.clone(),
        origin: batch.origin.clone(),
        batch_ref: ArtifactRef {
            path: batch_rel,
            digest: batch_digest,
        },
        review_request,
        digest: String::new(),
    };
    operation.digest = operation_identity_digest(&operation)?;

    let op_path = dir.join("operation.json");
    if op_path.exists() {
        let existing: PendingPostLandOperationV1 = serde_json::from_slice(&fs::read(&op_path)?)
            .with_context(|| format!("parse recorded operation {}", op_path.display()))?;
        if existing.digest != operation.digest {
            bail!(
                "post-land operation {operation_id} was already recorded with a conflicting \
                 identity"
            );
        }
        return Ok(existing);
    }

    atomic_write(&op_path, &serde_json::to_vec_pretty(&operation)?)
        .with_context(|| format!("write operation {}", op_path.display()))?;

    let journal_path = dir.join("journal.json");
    if !journal_path.exists() {
        write_journal(&journal_path, &PostLandJournal::empty(&operation_id))?;
    }
    Ok(operation)
}

/// Replay a recorded post-land operation, materializing one provenance-linked
/// Observation per follow-up. Replay is idempotent: it reuses the deterministic
/// Observation for each follow-up, never reopens a resolved one, and resumes from
/// the journal rather than duplicating completed work. An empty batch completes
/// the operation without creating any Observation.
pub fn replay_post_land_operation(
    project_root: &Path,
    operation_id: &str,
) -> Result<ReplayOutcome> {
    let dir = operation_dir(project_root, operation_id);
    let op_path = dir.join("operation.json");
    let operation: PendingPostLandOperationV1 = serde_json::from_slice(
        &fs::read(&op_path)
            .with_context(|| format!("read post-land operation {}", op_path.display()))?,
    )
    .with_context(|| format!("parse post-land operation {}", op_path.display()))?;

    if operation_identity_digest(&operation)? != operation.digest {
        bail!("post-land operation {operation_id} has a digest that does not match its identity");
    }

    let batch = load_verified_batch(project_root, &operation.batch_ref)?;
    if batch.origin != operation.origin {
        bail!("normalized batch origin does not match operation {operation_id}");
    }

    let journal_path = dir.join("journal.json");
    let mut journal = read_journal(&journal_path, operation_id)?;

    let mut created = 0usize;
    for follow_up in &batch.follow_ups {
        let observation_id = observation_id_for(operation_id, &follow_up.id);
        let frontmatter = follow_up_frontmatter(&operation.origin, follow_up);
        let body = follow_up_observation_body(&operation.origin, follow_up);
        let outcome = observations::ensure_provenance_observation(
            project_root,
            &observation_id,
            &frontmatter,
            &body,
        )?;
        if outcome.created() {
            created += 1;
        }
        if journal.receipt(&follow_up.id).is_none() {
            journal.follow_ups.push(FollowUpReceipt {
                follow_up_id: follow_up.id.clone(),
                observation_id,
            });
            write_journal(&journal_path, &journal)?;
        }
    }

    if !journal.completed {
        journal.completed = true;
        write_journal(&journal_path, &journal)?;
    }

    Ok(ReplayOutcome {
        operation_id: operation_id.to_string(),
        observations_created: created,
        follow_ups: batch.follow_ups.len(),
    })
}

/// Record and immediately replay a landed follow-up batch. This is the single
/// entry point the local land hook calls once a Merge Candidate is durably
/// merged.
pub fn process_landed_batch(
    project_root: &Path,
    batch: &NormalizedFollowUpBatchV1,
    review_request: Option<ArtifactRef>,
) -> Result<ReplayOutcome> {
    let operation = record_post_land_operation(project_root, batch, review_request)?;
    replay_post_land_operation(project_root, &operation.operation_id)
}

fn observation_id_for(operation_id: &str, follow_up_id: &str) -> String {
    format!("followup-{operation_id}-{}", sanitize_component(follow_up_id))
}

/// Reduce an untrusted follow-up id to a filename-safe component without the
/// length truncation an Observation title slug applies.
fn sanitize_component(raw: &str) -> String {
    let mut out: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if out.is_empty() {
        out.push_str("unnamed");
    }
    out
}

fn follow_up_frontmatter(
    origin: &PostLandOrigin,
    follow_up: &FollowUpDraftV1,
) -> ObservationFrontmatter {
    ObservationFrontmatter {
        kind: Some(LEARNER_FOLLOW_UP_KIND.to_string()),
        follow_up_id: Some(follow_up.id.clone()),
        work_item_id: Some(origin.work_item_id.clone()),
        attempt_id: Some(origin.attempt_id.clone()),
        merge_candidate_id: Some(origin.merge_candidate_id.clone()),
        merged_commit: Some(origin.merged_commit.clone()),
        derived_work_item_id: None,
    }
}

/// Render a self-contained Observation body: the follow-up summary, its corrective
/// context when present, and the origin identifiers, so the Observation stays
/// inspectable even after its origin artifacts are cleaned up.
fn follow_up_observation_body(origin: &PostLandOrigin, follow_up: &FollowUpDraftV1) -> String {
    let mut body = format!("# Follow-up: {}\n", follow_up.summary.trim());
    if let Some(context) = follow_up.corrective_context.as_ref() {
        body.push('\n');
        body.push_str(&context.to_execution_context());
        body.push('\n');
    }
    body.push_str(&format!(
        "\n## Origin\n\n\
         - Work Item: {}\n\
         - Attempt: {}\n\
         - Merge Candidate: {}\n\
         - Merged commit: {}\n",
        origin.work_item_id, origin.attempt_id, origin.merge_candidate_id, origin.merged_commit,
    ));
    body
}

fn load_verified_batch(
    project_root: &Path,
    reference: &ArtifactRef,
) -> Result<NormalizedFollowUpBatchV1> {
    if !reference.is_relative() {
        bail!("normalized batch reference {:?} is not relative", reference.path);
    }
    let path = project_root.join(&reference.path);
    let bytes =
        fs::read(&path).with_context(|| format!("read normalized batch {}", path.display()))?;
    let actual = content_digest(&bytes);
    if actual != reference.digest {
        bail!(
            "normalized batch at {} has digest {} but its reference expects {}",
            path.display(),
            actual,
            reference.digest
        );
    }
    serde_json::from_slice(&bytes)
        .with_context(|| format!("parse normalized batch {}", path.display()))
}

fn read_journal(path: &Path, operation_id: &str) -> Result<PostLandJournal> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .with_context(|| format!("parse post-land journal {}", path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            Ok(PostLandJournal::empty(operation_id))
        }
        Err(err) => Err(err).with_context(|| format!("read post-land journal {}", path.display())),
    }
}

fn write_journal(path: &Path, journal: &PostLandJournal) -> Result<()> {
    atomic_write(path, &serde_json::to_vec_pretty(journal)?)
        .with_context(|| format!("write post-land journal {}", path.display()))
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

    fn origin() -> PostLandOrigin {
        PostLandOrigin {
            work_item_id: "work-1".to_string(),
            attempt_id: "attempt-1".to_string(),
            merge_candidate_id: "attempt-1-merge-candidate".to_string(),
            merged_commit: "abc123".to_string(),
        }
    }

    fn batch_with(follow_ups: Vec<FollowUpDraftV1>) -> NormalizedFollowUpBatchV1 {
        NormalizedFollowUpBatchV1 {
            schema_version: NormalizedFollowUpBatchV1::SCHEMA_VERSION,
            source: FollowUpSource::Learner,
            origin: origin(),
            learning_summary: "learned".to_string(),
            follow_ups,
        }
    }

    fn draft(id: &str, summary: &str) -> FollowUpDraftV1 {
        FollowUpDraftV1 {
            id: id.to_string(),
            summary: summary.to_string(),
            corrective: false,
            corrective_context: None,
            evidence: Vec::new(),
        }
    }

    #[test]
    fn operation_round_trips_and_identity_digest_is_stable() {
        let tmp = tempfile::TempDir::new().unwrap();
        let batch = batch_with(vec![draft("fu-1", "Restore the retry cap")]);
        let operation = record_post_land_operation(tmp.path(), &batch, None).unwrap();

        let json = serde_json::to_string_pretty(&operation).unwrap();
        let restored: PendingPostLandOperationV1 = serde_json::from_str(&json).unwrap();
        assert_eq!(operation, restored);
        assert_eq!(
            operation.operation_id,
            "work-1-attempt-1-merge-candidate",
            "operation id is deterministic from the origin"
        );
        assert!(operation.digest.starts_with("sha256:"));
        assert_eq!(
            operation_identity_digest(&operation).unwrap(),
            operation.digest
        );
    }

    #[test]
    fn record_persists_batch_operation_and_journal() {
        let tmp = tempfile::TempDir::new().unwrap();
        let batch = batch_with(vec![draft("fu-1", "one")]);
        let operation = record_post_land_operation(tmp.path(), &batch, None).unwrap();

        let dir = operation_dir(tmp.path(), &operation.operation_id);
        assert!(dir.join("operation.json").exists());
        assert!(dir.join("batch.json").exists());
        assert!(dir.join("journal.json").exists());

        // Re-recording the same origin returns the existing operation.
        let again = record_post_land_operation(tmp.path(), &batch, None).unwrap();
        assert_eq!(again, operation);
    }

    #[test]
    fn replay_materializes_one_provenance_observation_per_follow_up() {
        let tmp = tempfile::TempDir::new().unwrap();
        let batch = batch_with(vec![draft("fu-1", "one"), draft("fu-2", "two")]);
        let outcome = process_landed_batch(tmp.path(), &batch, None).unwrap();
        assert_eq!(outcome.observations_created, 2);
        assert_eq!(outcome.follow_ups, 2);

        let obs_dir = tmp.path().join(".fluent/observations");
        let count = fs::read_dir(&obs_dir)
            .unwrap()
            .filter(|e| e.as_ref().unwrap().file_type().unwrap().is_file())
            .count();
        assert_eq!(count, 2);

        let content = fs::read_to_string(
            obs_dir.join("followup-work-1-attempt-1-merge-candidate-fu-1.md"),
        )
        .unwrap();
        assert!(content.contains("work-item-id: work-1"));
        assert!(content.contains("attempt-id: attempt-1"));
        assert!(content.contains("merge-candidate-id: attempt-1-merge-candidate"));
        assert!(content.contains("merged-commit: abc123"));
        assert!(content.contains("follow-up-id: fu-1"));
    }

    #[test]
    fn replay_is_idempotent() {
        let tmp = tempfile::TempDir::new().unwrap();
        let batch = batch_with(vec![draft("fu-1", "one")]);
        let operation = record_post_land_operation(tmp.path(), &batch, None).unwrap();

        let first = replay_post_land_operation(tmp.path(), &operation.operation_id).unwrap();
        assert_eq!(first.observations_created, 1);
        let second = replay_post_land_operation(tmp.path(), &operation.operation_id).unwrap();
        assert_eq!(
            second.observations_created, 0,
            "a second replay creates no new Observation"
        );

        let count = fs::read_dir(tmp.path().join(".fluent/observations"))
            .unwrap()
            .filter(|e| e.as_ref().unwrap().file_type().unwrap().is_file())
            .count();
        assert_eq!(count, 1);
    }

    #[test]
    fn empty_batch_completes_without_creating_observations() {
        let tmp = tempfile::TempDir::new().unwrap();
        let batch = batch_with(Vec::new());
        let outcome = process_landed_batch(tmp.path(), &batch, None).unwrap();
        assert_eq!(outcome.observations_created, 0);
        assert_eq!(outcome.follow_ups, 0);

        assert!(
            !tmp.path().join(".fluent/observations").exists(),
            "an empty batch creates no Observation backlog"
        );

        let journal_path = operation_dir(tmp.path(), &outcome.operation_id).join("journal.json");
        let journal: PostLandJournal =
            serde_json::from_slice(&fs::read(&journal_path).unwrap()).unwrap();
        assert!(journal.completed, "an empty batch still marks the journal complete");
        assert!(journal.follow_ups.is_empty());
    }

    #[test]
    fn resolved_observation_is_not_reopened_on_replay() {
        let tmp = tempfile::TempDir::new().unwrap();
        let batch = batch_with(vec![draft("fu-1", "one")]);
        let operation = record_post_land_operation(tmp.path(), &batch, None).unwrap();
        replay_post_land_operation(tmp.path(), &operation.operation_id).unwrap();

        let observation_id = observation_id_for(&operation.operation_id, "fu-1");
        observations::resolve(tmp.path(), &observation_id, Some("handled".to_string())).unwrap();

        // Replaying must not reopen the resolved Observation.
        let outcome = replay_post_land_operation(tmp.path(), &operation.operation_id).unwrap();
        assert_eq!(outcome.observations_created, 0);
        assert!(
            tmp.path()
                .join(format!(".fluent/observations/resolved/{observation_id}.md"))
                .exists()
        );
        assert!(
            !tmp.path()
                .join(format!(".fluent/observations/{observation_id}.md"))
                .exists(),
            "a resolved follow-up Observation stays resolved"
        );
    }

    #[test]
    fn replay_rejects_tampered_batch() {
        let tmp = tempfile::TempDir::new().unwrap();
        let batch = batch_with(vec![draft("fu-1", "one")]);
        let operation = record_post_land_operation(tmp.path(), &batch, None).unwrap();

        // Alter the persisted batch so it no longer matches the reference digest.
        let batch_path = tmp.path().join(&operation.batch_ref.path);
        fs::write(&batch_path, b"{\"schema_version\":1,\"source\":\"learner\"}").unwrap();

        let err = replay_post_land_operation(tmp.path(), &operation.operation_id).unwrap_err();
        assert!(
            err.to_string().contains("digest"),
            "a tampered batch is rejected by digest: {err}"
        );
    }

    #[test]
    fn from_learner_handoff_rejects_origin_mismatch() {
        let handoff = sample_handoff();
        let mut mismatched = origin();
        mismatched.merge_candidate_id = "some-other-candidate".to_string();
        assert!(NormalizedFollowUpBatchV1::from_learner_handoff(&handoff, mismatched).is_err());
    }

    #[test]
    fn from_learner_handoff_normalizes_matching_origin() {
        let handoff = sample_handoff();
        let origin = PostLandOrigin {
            work_item_id: handoff.source_work_item_id.clone(),
            attempt_id: handoff.source_attempt_id.clone(),
            merge_candidate_id: handoff.source_merge_candidate_id.clone().unwrap(),
            merged_commit: "abc123".to_string(),
        };
        let batch = NormalizedFollowUpBatchV1::from_learner_handoff(&handoff, origin).unwrap();
        assert_eq!(batch.source, FollowUpSource::Learner);
        assert_eq!(batch.follow_ups.len(), handoff.follow_ups.len());
    }
}
