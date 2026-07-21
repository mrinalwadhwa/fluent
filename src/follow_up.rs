//! Versioned, type-only contracts for the learning follow-up flywheel.
//!
//! These structures pin the serialized shape of a learner handoff and the
//! follow-up drafts and learning record it carries. Producing and consuming
//! handoffs lands in later Work Items; this module fixes the vocabulary and the
//! on-disk format now so the scheduler and Learner branches cannot diverge.

use anyhow::{Context, Result, bail};
use rustix::fs::{FlockOperation, flock};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};

use crate::atomic_write::atomic_write;
use crate::config::{self, CorrectionSource, FrozenFollowUpPolicy};
use crate::observations::{self, LEARNER_FOLLOW_UP_KIND, ObservationFrontmatter};
use crate::work_model::{
    CorrectiveContext, DerivedProvenance, ExecutionAuthority, WorkItem, WorkLineage, WorkModelStore,
    WorkModelStorageError,
};

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
    /// The result the corrective Work must produce. Required for a corrective
    /// follow-up to pass the host gate.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub expected_result: String,
    /// Decisions the proposal leaves open. A corrective follow-up must resolve
    /// every one; a non-empty set forces the follow-up to stay Observation-only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unresolved_decisions: Vec<String>,
    /// The trusted authority this corrective follow-up derives from. The host
    /// resolves it against the project tree before accepting the follow-up as
    /// corrective.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority: Option<AuthorityLocator>,
    /// Supporting evidence, relative and digest-bearing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<ArtifactRef>,
}

/// The trusted authority namespace a corrective follow-up cites as the basis for
/// bypassing the usual brief, behaviors, approach, and plan conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthorityKind {
    /// A behavior statement in `documentation/behaviors.md`.
    BehaviorStatement,
    /// An applicable instruction in a tracked `AGENTS.md`.
    AgentsInstruction,
    /// A committed entry under `.fluent/expertise/`.
    ExpertiseEntry,
}

/// A stable, digest-matched locator to the committed authority a corrective
/// follow-up derives from. The host resolves it against the project tree and
/// rejects the follow-up when the cited authority is missing, moved outside its
/// namespace, tampered with, or drifted from the anchor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorityLocator {
    pub kind: AuthorityKind,
    /// Path to the authoritative file, relative to the project root.
    pub path: String,
    /// The exact authoritative text this follow-up derives from. It must still
    /// be present in the referenced file for the locator to be fresh.
    pub anchor: String,
    /// `sha256:` digest over the anchor bytes, so a transported locator is
    /// tamper-evident and self-consistent.
    pub digest: String,
}

/// How the corrective host gate classified a materialized follow-up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FollowUpClassification {
    /// The follow-up satisfies every corrective-work criterion and may promote
    /// to derived Work under policy.
    Corrective,
    /// The follow-up stays an Observation only, for the recorded reason.
    ObservationOnly { reason: String },
}

impl FollowUpClassification {
    pub fn is_corrective(&self) -> bool {
        matches!(self, Self::Corrective)
    }
}

/// Classify a materialized follow-up through the source-neutral corrective host
/// gate. A follow-up is corrective only when it claims to be, carries a complete
/// corrective context and expected result, leaves no unresolved decision, and
/// cites a trusted authority that still resolves fresh against the project tree.
/// Any incomplete, unsupported, unresolved, stale, or mis-namespaced context
/// downgrades it to Observation-only. The gate is deterministic and neutral to
/// whether the follow-up came from a Learner or a post-merge review.
pub fn classify_follow_up(
    project_root: &Path,
    follow_up: &FollowUpDraftV1,
) -> FollowUpClassification {
    use FollowUpClassification::ObservationOnly;
    if !follow_up.corrective {
        return ObservationOnly {
            reason: "follow-up is not marked corrective".to_string(),
        };
    }
    let Some(context) = follow_up.corrective_context.as_ref() else {
        return ObservationOnly {
            reason: "corrective follow-up carries no corrective context".to_string(),
        };
    };
    if let Err(error) = context.validate() {
        return ObservationOnly {
            reason: format!("incomplete corrective context: {error}"),
        };
    }
    if follow_up.expected_result.trim().is_empty() {
        return ObservationOnly {
            reason: "corrective follow-up states no expected result".to_string(),
        };
    }
    if !follow_up.unresolved_decisions.is_empty() {
        return ObservationOnly {
            reason: format!(
                "{} unresolved decision(s) remain",
                follow_up.unresolved_decisions.len()
            ),
        };
    }
    let Some(authority) = follow_up.authority.as_ref() else {
        return ObservationOnly {
            reason: "corrective follow-up cites no authority".to_string(),
        };
    };
    if let Err(reason) = verify_authority(project_root, authority) {
        return ObservationOnly { reason };
    }
    FollowUpClassification::Corrective
}

/// Whether an authority locator's path lives in the trusted namespace its kind
/// requires.
fn authority_namespace_ok(kind: AuthorityKind, path: &str) -> bool {
    match kind {
        AuthorityKind::BehaviorStatement => path == "documentation/behaviors.md",
        AuthorityKind::AgentsInstruction => {
            Path::new(path).file_name().and_then(|name| name.to_str()) == Some("AGENTS.md")
        }
        AuthorityKind::ExpertiseEntry => path.starts_with(".fluent/expertise/"),
    }
}

/// Resolve an authority locator against the project tree. It must be relative,
/// live in its kind's trusted namespace, be self-consistent with its digest, and
/// its anchor text must still be present in the referenced file. A missing file
/// or a vanished anchor is treated as stale — the correction can no longer point
/// to live authority, so its follow-up stays Observation-only.
fn verify_authority(project_root: &Path, authority: &AuthorityLocator) -> Result<(), String> {
    if !Path::new(&authority.path).is_relative() {
        return Err(format!("authority path {:?} is not relative", authority.path));
    }
    if !authority_namespace_ok(authority.kind, &authority.path) {
        return Err(format!(
            "authority path {:?} is not in the {:?} trusted namespace",
            authority.path, authority.kind
        ));
    }
    if authority.digest != content_digest(authority.anchor.as_bytes()) {
        return Err("authority digest does not match its anchor".to_string());
    }
    let full = project_root.join(&authority.path);
    let content = match fs::read_to_string(&full) {
        Ok(content) => content,
        Err(_) => return Err(format!("authority {:?} is not present", authority.path)),
    };
    if !content.contains(&authority.anchor) {
        return Err(format!(
            "authority {:?} no longer contains the cited text",
            authority.path
        ));
    }
    Ok(())
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
}

/// What one follow-up produced: its materialized Observation, the corrective
/// classification, the policy frozen when it first validated as corrective, and
/// the derived Work Item and queue disposition it promoted into. Each field is
/// recorded once and reused on retry so processing converges exactly once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FollowUpReceipt {
    pub follow_up_id: String,
    /// The deterministic Observation id this follow-up materialized into.
    pub observation_id: String,
    /// Whether the corrective host gate classified this follow-up as corrective.
    /// Recorded so a retry reuses the first classification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corrective: Option<bool>,
    /// The follow-up policy frozen when this follow-up first validated as
    /// corrective, recorded before any Work is created. Retries reuse it even
    /// after configuration changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_policy: Option<FrozenFollowUpPolicy>,
    /// The derived Work Item this corrective follow-up promoted into, once one
    /// exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derived_work_item_id: Option<String>,
    /// Whether the derived Work Item was enqueued on the regular Work Queue.
    #[serde(default, skip_serializing_if = "is_false")]
    pub queued: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// The result of replaying a post-land operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayOutcome {
    pub operation_id: String,
    /// Observations newly created by this replay (0 on an idempotent re-run).
    pub observations_created: usize,
    /// Follow-ups the batch carries.
    pub follow_ups: usize,
    /// Derived Work Items newly created by this replay (0 on an idempotent
    /// re-run).
    pub work_items_created: usize,
    /// Derived Work Items newly enqueued by this replay (0 on an idempotent
    /// re-run).
    pub work_items_queued: usize,
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

    // Serialize concurrent processors of the same operation so each stage runs
    // once and every processor converges on the same effects rather than racing
    // to create a duplicate Observation, Work Item, or queue entry.
    let _lock = lock_operation(&dir)?;

    let journal_path = dir.join("journal.json");
    let mut journal = read_journal(&journal_path, operation_id)?;
    let store = WorkModelStore::new(project_root);

    let mut totals = ReplayTotals::default();
    for follow_up in &batch.follow_ups {
        process_one_follow_up(
            &store,
            project_root,
            &operation,
            batch.source,
            follow_up,
            &journal_path,
            &mut journal,
            &mut totals,
        )?;
    }

    if !journal.completed {
        journal.completed = true;
        write_journal(&journal_path, &journal)?;
    }

    Ok(ReplayOutcome {
        operation_id: operation_id.to_string(),
        observations_created: totals.observations_created,
        follow_ups: batch.follow_ups.len(),
        work_items_created: totals.work_items_created,
        work_items_queued: totals.work_items_queued,
    })
}

/// Newly produced effects accumulated across a single replay, used to report an
/// idempotent re-run as a no-op.
#[derive(Default)]
struct ReplayTotals {
    observations_created: usize,
    work_items_created: usize,
    work_items_queued: usize,
}

/// Drive one follow-up through its ordered stages, resuming from the journal:
/// materialize its Observation, classify it through the corrective host gate,
/// and — when corrective — freeze the follow-up policy before deriving proposed
/// or execution-ready Work and enqueuing it under lineage budget. Each completed
/// stage is journaled so a retry resumes rather than repeats it.
#[allow(clippy::too_many_arguments)]
fn process_one_follow_up(
    store: &WorkModelStore,
    project_root: &Path,
    operation: &PendingPostLandOperationV1,
    source: FollowUpSource,
    follow_up: &FollowUpDraftV1,
    journal_path: &Path,
    journal: &mut PostLandJournal,
    totals: &mut ReplayTotals,
) -> Result<()> {
    let operation_id = &operation.operation_id;
    let observation_id = observation_id_for(operation_id, &follow_up.id);

    // Stage 1: materialize the provenance-linked Observation.
    let frontmatter = follow_up_frontmatter(&operation.origin, follow_up);
    let body = follow_up_observation_body(&operation.origin, follow_up);
    let outcome = observations::ensure_provenance_observation(
        project_root,
        &observation_id,
        &frontmatter,
        &body,
    )?;
    if outcome.created() {
        totals.observations_created += 1;
    }
    let index = ensure_receipt(journal, journal_path, &follow_up.id, &observation_id)?;

    // Stage 2: classify through the corrective host gate. The first
    // classification is recorded and reused on retry.
    if journal.follow_ups[index].corrective.is_none() {
        let corrective = classify_follow_up(project_root, follow_up).is_corrective();
        journal.follow_ups[index].corrective = Some(corrective);
        write_journal(journal_path, journal)?;
    }
    if journal.follow_ups[index].corrective != Some(true) {
        // Non-corrective follow-ups stay Observation-only.
        return Ok(());
    }

    // Stage 3: freeze the follow-up policy before any Work, authorization,
    // lineage, priority, or queue decision. A retry reuses the recorded policy
    // even after configuration changes.
    if journal.follow_ups[index].resolved_policy.is_none() {
        let policy = config::resolve_follow_up_policy(project_root)?.freeze(correction_source(source));
        journal.follow_ups[index].resolved_policy = Some(policy);
        write_journal(journal_path, journal)?;
    }
    let policy = journal.follow_ups[index]
        .resolved_policy
        .clone()
        .expect("resolved policy was just recorded");

    // Stage 4: derive proposed or execution-ready Work under lineage budget.
    let derived_id = derived_work_item_id(operation_id, &follow_up.id);
    if journal.follow_ups[index].derived_work_item_id.is_none() {
        let created = ensure_derived_work(
            store,
            &operation.origin,
            &observation_id,
            &derived_id,
            follow_up,
            &policy,
        )?;
        if created {
            totals.work_items_created += 1;
        }
        journal.follow_ups[index].derived_work_item_id = Some(derived_id.clone());
        write_journal(journal_path, journal)?;
    }

    // Stage 5: enqueue an authorized execution-ready descendant exactly once,
    // through its durable enqueue intent.
    if !journal.follow_ups[index].queued {
        let item = store.read_work_item(&derived_id)?;
        if item.authorization.is_execution_ready()
            && let Some(intent) = item.pending_enqueue.as_ref()
        {
            crate::queue::ensure_dispatch(
                project_root,
                &derived_id,
                &intent.origin_operation_id,
                intent.priority,
            )?;
            totals.work_items_queued += 1;
            journal.follow_ups[index].queued = true;
            write_journal(journal_path, journal)?;
        }
    }

    Ok(())
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

/// The deterministic Work Item id a corrective follow-up promotes into, so a
/// replay reuses the same Work rather than deriving a duplicate.
fn derived_work_item_id(operation_id: &str, follow_up_id: &str) -> String {
    format!("derived-{operation_id}-{}", sanitize_component(follow_up_id))
}

fn correction_source(source: FollowUpSource) -> CorrectionSource {
    match source {
        FollowUpSource::Learner => CorrectionSource::Learner,
        FollowUpSource::PostMerge => CorrectionSource::PostMerge,
    }
}

/// Ensure a journal receipt exists for a follow-up and return its index. A new
/// receipt records only the materialized Observation; later stages fill in the
/// classification, frozen policy, derived Work, and queue disposition.
fn ensure_receipt(
    journal: &mut PostLandJournal,
    journal_path: &Path,
    follow_up_id: &str,
    observation_id: &str,
) -> Result<usize> {
    if let Some(index) = journal
        .follow_ups
        .iter()
        .position(|receipt| receipt.follow_up_id == follow_up_id)
    {
        return Ok(index);
    }
    journal.follow_ups.push(FollowUpReceipt {
        follow_up_id: follow_up_id.to_string(),
        observation_id: observation_id.to_string(),
        corrective: None,
        resolved_policy: None,
        derived_work_item_id: None,
        queued: false,
    });
    write_journal(journal_path, journal)?;
    Ok(journal.follow_ups.len() - 1)
}

/// Create the derived corrective Work Item for a follow-up, or reuse an existing
/// one. The Work joins its originating Work Item's lineage root. In execute mode
/// it is authorized automatically while lineage budget remains, charging the
/// lineage once; in propose mode or with the budget exhausted it stays proposed.
/// Returns whether a new Work Item was created.
fn ensure_derived_work(
    store: &WorkModelStore,
    origin: &PostLandOrigin,
    observation_id: &str,
    derived_id: &str,
    follow_up: &FollowUpDraftV1,
    policy: &FrozenFollowUpPolicy,
) -> Result<bool> {
    // Idempotent: an already-derived Work Item is reused as-is.
    match store.read_work_item(derived_id) {
        Ok(_) => return Ok(false),
        Err(WorkModelStorageError::ReadFile { source, .. })
            if source.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    let context = follow_up
        .corrective_context
        .clone()
        .context("corrective follow-up lost its corrective context before Work creation")?;

    let provenance = DerivedProvenance {
        observation_id: Some(observation_id.to_string()),
        work_item_id: Some(origin.work_item_id.clone()),
        attempt_id: Some(origin.attempt_id.clone()),
        merge_candidate_id: Some(origin.merge_candidate_id.clone()),
        merged_commit: Some(origin.merged_commit.clone()),
    };

    // The derived Work joins its originating Work Item's lineage root.
    let origin_item = store.read_work_item(&origin.work_item_id)?;
    let root_id = origin_item.lineage.root_id(&origin_item.id).to_string();
    let lineage = WorkLineage::descendant_of(root_id.clone(), Some(policy.descendant_limit));

    // Execute mode authorizes automatically while lineage budget remains;
    // otherwise the descendant stays proposed for an explicit human decision.
    let ready_authority = if policy.is_execute()
        && WorkLineage::can_authorize_descendant(
            count_charged_descendants(store, &root_id, derived_id)?,
            policy.descendant_limit,
        ) {
        Some(ExecutionAuthority::Automatic)
    } else {
        None
    };

    let mut item = WorkItem::derived_corrective(
        derived_id.to_string(),
        follow_up.summary.trim().to_string(),
        provenance,
        context,
        lineage,
        ready_authority,
    )?;
    // Record the durable enqueue intent up front so an execution-ready
    // descendant enqueues now and a proposed one enqueues on human
    // authorization, both keyed by the same stable origin.
    item.set_enqueue_intent(policy.priority, derived_id.to_string());
    store.create_work_item(&item)?;
    Ok(true)
}

/// Count the Work Items already charged against a lineage's autonomous
/// descendant budget, excluding the descendant being resolved.
fn count_charged_descendants(
    store: &WorkModelStore,
    root_id: &str,
    exclude_id: &str,
) -> Result<u32> {
    let mut count = 0u32;
    for item in store.list_work_items()? {
        if item.id == exclude_id {
            continue;
        }
        if item.origin.is_derived()
            && item.lineage.charged
            && item.lineage.root_id(&item.id) == root_id
        {
            count += 1;
        }
    }
    Ok(count)
}

/// Take a blocking exclusive lock on a post-land operation so concurrent
/// processors serialize and converge on the same effects.
fn lock_operation(dir: &Path) -> Result<File> {
    let path = dir.join("operation.lock");
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .open(&path)
        .with_context(|| format!("open operation lock {}", path.display()))?;
    flock(&file, FlockOperation::LockExclusive)
        .map_err(std::io::Error::from)
        .with_context(|| format!("lock operation {}", path.display()))?;
    Ok(file)
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
                ..Default::default()
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
            ..Default::default()
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

    // --- Corrective host gate (Step 2: B1a) ---

    fn corrective_context_sample() -> CorrectiveContext {
        CorrectiveContext {
            objective: "Restore the retry guard".to_string(),
            requirement: "Retries stop after the configured cap".to_string(),
            evidence: "Merged commit abc123 removed the cap check".to_string(),
            included_scope: "src/retry.rs".to_string(),
            excluded_scope: "unrelated backoff tuning".to_string(),
            verification: "cargo test retry::cap_is_enforced".to_string(),
        }
    }

    fn locator(kind: AuthorityKind, path: &str, anchor: &str) -> AuthorityLocator {
        AuthorityLocator {
            kind,
            path: path.to_string(),
            anchor: anchor.to_string(),
            digest: content_digest(anchor.as_bytes()),
        }
    }

    fn corrective_follow_up(authority: Option<AuthorityLocator>) -> FollowUpDraftV1 {
        FollowUpDraftV1 {
            id: "fu-1".to_string(),
            summary: "Restore the retry cap".to_string(),
            corrective: true,
            corrective_context: Some(corrective_context_sample()),
            expected_result: "The retry cap is enforced again".to_string(),
            unresolved_decisions: Vec::new(),
            authority,
            evidence: Vec::new(),
        }
    }

    /// Write an authoritative file into the project tree so a locator over it
    /// resolves.
    fn write_authority(root: &Path, rel: &str, contents: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn gate_accepts_complete_fresh_trusted_expertise_authority() {
        let tmp = tempfile::TempDir::new().unwrap();
        let anchor = "Cap enforcement belongs in retry.rs";
        write_authority(
            tmp.path(),
            ".fluent/expertise/retry.md",
            &format!("# Retry\n\n{anchor}\n"),
        );
        let follow_up = corrective_follow_up(Some(locator(
            AuthorityKind::ExpertiseEntry,
            ".fluent/expertise/retry.md",
            anchor,
        )));
        assert_eq!(
            classify_follow_up(tmp.path(), &follow_up),
            FollowUpClassification::Corrective
        );
    }

    #[test]
    fn gate_accepts_behavior_and_agents_namespaces() {
        let tmp = tempfile::TempDir::new().unwrap();
        let behavior_anchor = "THE SYSTEM SHALL enforce the retry cap";
        write_authority(
            tmp.path(),
            "documentation/behaviors.md",
            &format!("### B1\n\n{behavior_anchor}\n"),
        );
        let agents_anchor = "Always enforce the configured retry cap";
        write_authority(tmp.path(), "AGENTS.md", &format!("- {agents_anchor}\n"));

        let behavior_follow_up = corrective_follow_up(Some(locator(
            AuthorityKind::BehaviorStatement,
            "documentation/behaviors.md",
            behavior_anchor,
        )));
        let agents_follow_up = corrective_follow_up(Some(locator(
            AuthorityKind::AgentsInstruction,
            "AGENTS.md",
            agents_anchor,
        )));
        assert!(classify_follow_up(tmp.path(), &behavior_follow_up).is_corrective());
        assert!(classify_follow_up(tmp.path(), &agents_follow_up).is_corrective());
    }

    #[test]
    fn gate_downgrades_non_corrective_and_incomplete_context() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut not_corrective = corrective_follow_up(None);
        not_corrective.corrective = false;
        assert!(!classify_follow_up(tmp.path(), &not_corrective).is_corrective());

        let mut incomplete = corrective_follow_up(None);
        incomplete.corrective_context = None;
        assert!(!classify_follow_up(tmp.path(), &incomplete).is_corrective());
    }

    #[test]
    fn gate_downgrades_missing_expected_result_and_unresolved_decisions() {
        let tmp = tempfile::TempDir::new().unwrap();
        write_authority(tmp.path(), ".fluent/expertise/retry.md", "anchor text");
        let base = locator(AuthorityKind::ExpertiseEntry, ".fluent/expertise/retry.md", "anchor text");

        let mut no_result = corrective_follow_up(Some(base.clone()));
        no_result.expected_result = "   ".to_string();
        assert!(!classify_follow_up(tmp.path(), &no_result).is_corrective());

        let mut unresolved = corrective_follow_up(Some(base));
        unresolved.unresolved_decisions = vec!["pick a backoff curve".to_string()];
        assert!(!classify_follow_up(tmp.path(), &unresolved).is_corrective());
    }

    #[test]
    fn gate_downgrades_missing_stale_tampered_or_mis_namespaced_authority() {
        let tmp = tempfile::TempDir::new().unwrap();
        let anchor = "Cap enforcement belongs in retry.rs";

        // No authority at all.
        assert!(!classify_follow_up(tmp.path(), &corrective_follow_up(None)).is_corrective());

        // Authority cited but the file is absent.
        let missing = corrective_follow_up(Some(locator(
            AuthorityKind::ExpertiseEntry,
            ".fluent/expertise/retry.md",
            anchor,
        )));
        assert!(!classify_follow_up(tmp.path(), &missing).is_corrective());

        // File present but the anchor has drifted away (stale).
        write_authority(tmp.path(), ".fluent/expertise/retry.md", "# Retry\n\nunrelated text\n");
        let stale = corrective_follow_up(Some(locator(
            AuthorityKind::ExpertiseEntry,
            ".fluent/expertise/retry.md",
            anchor,
        )));
        assert!(!classify_follow_up(tmp.path(), &stale).is_corrective());

        // Digest does not match the anchor (tampered in transport).
        write_authority(tmp.path(), ".fluent/expertise/retry.md", &format!("{anchor}\n"));
        let mut tampered = corrective_follow_up(Some(locator(
            AuthorityKind::ExpertiseEntry,
            ".fluent/expertise/retry.md",
            anchor,
        )));
        tampered.authority.as_mut().unwrap().digest = "sha256:0".to_string();
        assert!(!classify_follow_up(tmp.path(), &tampered).is_corrective());

        // Right anchor and digest, but the path is outside the kind's namespace.
        write_authority(tmp.path(), "src/retry.rs", &format!("// {anchor}\n"));
        let mis_namespaced = corrective_follow_up(Some(locator(
            AuthorityKind::ExpertiseEntry,
            "src/retry.rs",
            anchor,
        )));
        assert!(!classify_follow_up(tmp.path(), &mis_namespaced).is_corrective());
    }

    // --- Corrective promotion into derived Work (Step 2: B2, B3, B4, B5) ---

    fn seed_root_work_item(root: &Path) {
        let store = WorkModelStore::new(root);
        store
            .create_work_item(&WorkItem::planned("work-1", "Root work"))
            .unwrap();
    }

    fn corrective_batch(source: FollowUpSource, authority: Option<AuthorityLocator>) -> NormalizedFollowUpBatchV1 {
        NormalizedFollowUpBatchV1 {
            schema_version: NormalizedFollowUpBatchV1::SCHEMA_VERSION,
            source,
            origin: origin(),
            learning_summary: "learned".to_string(),
            follow_ups: vec![corrective_follow_up(authority)],
        }
    }

    fn fresh_expertise_authority(root: &Path) -> AuthorityLocator {
        let anchor = "Cap enforcement belongs in retry.rs";
        write_authority(root, ".fluent/expertise/retry.md", &format!("{anchor}\n"));
        locator(AuthorityKind::ExpertiseEntry, ".fluent/expertise/retry.md", anchor)
    }

    fn write_project_policy(root: &Path, yaml: &str) {
        let dir = root.join(".fluent");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("config.yaml"), yaml).unwrap();
    }

    const DERIVED_ID: &str = "derived-work-1-attempt-1-merge-candidate-fu-1";

    #[test]
    fn non_corrective_follow_up_stays_observation_only() {
        let tmp = tempfile::TempDir::new().unwrap();
        seed_root_work_item(tmp.path());
        // A follow-up with no authority never passes the gate.
        let batch = corrective_batch(FollowUpSource::Learner, None);
        let outcome = process_landed_batch(tmp.path(), &batch, None).unwrap();
        assert_eq!(outcome.observations_created, 1);
        assert_eq!(outcome.work_items_created, 0);
        let store = WorkModelStore::new(tmp.path());
        assert!(store.read_work_item(DERIVED_ID).is_err());
    }

    #[test]
    fn propose_mode_creates_proposed_unqueued_descendant() {
        let tmp = tempfile::TempDir::new().unwrap();
        seed_root_work_item(tmp.path());
        let authority = fresh_expertise_authority(tmp.path());
        let batch = corrective_batch(FollowUpSource::Learner, Some(authority));

        let outcome = process_landed_batch(tmp.path(), &batch, None).unwrap();
        assert_eq!(outcome.work_items_created, 1);
        assert_eq!(outcome.work_items_queued, 0);

        let store = WorkModelStore::new(tmp.path());
        let derived = store.read_work_item(DERIVED_ID).unwrap();
        assert!(derived.authorization.is_proposed());
        assert!(derived.origin.is_derived());
        assert_eq!(derived.lineage.root_id.as_deref(), Some("work-1"));
        assert_eq!(
            derived.origin.provenance().unwrap().observation_id.as_deref(),
            Some("followup-work-1-attempt-1-merge-candidate-fu-1")
        );
        assert!(
            crate::queue::read_ledger(tmp.path(), DERIVED_ID)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn execute_mode_with_budget_creates_ready_queued_descendant() {
        let tmp = tempfile::TempDir::new().unwrap();
        seed_root_work_item(tmp.path());
        write_project_policy(tmp.path(), "follow-up:\n  mode: execute\n");
        let authority = fresh_expertise_authority(tmp.path());
        let batch = corrective_batch(FollowUpSource::Learner, Some(authority));

        let outcome = process_landed_batch(tmp.path(), &batch, None).unwrap();
        assert_eq!(outcome.work_items_created, 1);
        assert_eq!(outcome.work_items_queued, 1);

        let store = WorkModelStore::new(tmp.path());
        let derived = store.read_work_item(DERIVED_ID).unwrap();
        assert!(derived.authorization.is_execution_ready());
        assert!(derived.lineage.charged, "an authorized descendant charges its lineage");

        let ledger = crate::queue::read_ledger(tmp.path(), DERIVED_ID)
            .unwrap()
            .unwrap();
        assert!(ledger.active().is_some(), "execute mode enqueues the descendant");
    }

    #[test]
    fn execute_mode_at_lineage_limit_stays_proposed() {
        let tmp = tempfile::TempDir::new().unwrap();
        seed_root_work_item(tmp.path());
        write_project_policy(
            tmp.path(),
            "follow-up:\n  mode: execute\n  descendant-limit: 1\n",
        );
        // Pre-charge the single descendant the lineage budget allows.
        let store = WorkModelStore::new(tmp.path());
        let existing = WorkItem::derived_corrective(
            "derived-existing",
            "Existing descendant",
            DerivedProvenance {
                work_item_id: Some("work-1".to_string()),
                ..Default::default()
            },
            corrective_context_sample(),
            WorkLineage::descendant_of("work-1", Some(1)),
            Some(ExecutionAuthority::Automatic),
        )
        .unwrap();
        store.create_work_item(&existing).unwrap();

        let authority = fresh_expertise_authority(tmp.path());
        let batch = corrective_batch(FollowUpSource::Learner, Some(authority));
        let outcome = process_landed_batch(tmp.path(), &batch, None).unwrap();

        assert_eq!(outcome.work_items_created, 1);
        assert_eq!(outcome.work_items_queued, 0, "an exhausted budget does not enqueue");
        let derived = store.read_work_item(DERIVED_ID).unwrap();
        assert!(derived.authorization.is_proposed());
        assert!(!derived.lineage.charged);
    }

    #[test]
    fn corrective_promotion_is_idempotent_across_replay() {
        let tmp = tempfile::TempDir::new().unwrap();
        seed_root_work_item(tmp.path());
        write_project_policy(tmp.path(), "follow-up:\n  mode: execute\n");
        let authority = fresh_expertise_authority(tmp.path());
        let batch = corrective_batch(FollowUpSource::Learner, Some(authority));

        let operation = record_post_land_operation(tmp.path(), &batch, None).unwrap();
        let first = replay_post_land_operation(tmp.path(), &operation.operation_id).unwrap();
        assert_eq!(first.work_items_created, 1);
        assert_eq!(first.work_items_queued, 1);

        let second = replay_post_land_operation(tmp.path(), &operation.operation_id).unwrap();
        assert_eq!(second.work_items_created, 0, "replay derives no duplicate Work");
        assert_eq!(second.work_items_queued, 0, "replay adds no duplicate dispatch");

        let ledger = crate::queue::read_ledger(tmp.path(), DERIVED_ID)
            .unwrap()
            .unwrap();
        assert_eq!(ledger.dispatches.len(), 1, "exactly one queue entry survives replay");
    }

    #[test]
    fn corrective_retry_reuses_frozen_policy_after_config_change() {
        let tmp = tempfile::TempDir::new().unwrap();
        seed_root_work_item(tmp.path());
        write_project_policy(tmp.path(), "follow-up:\n  mode: execute\n");
        let authority = fresh_expertise_authority(tmp.path());
        let batch = corrective_batch(FollowUpSource::Learner, Some(authority));

        let operation = record_post_land_operation(tmp.path(), &batch, None).unwrap();
        replay_post_land_operation(tmp.path(), &operation.operation_id).unwrap();

        // The operator changes the policy to propose after the first processing.
        write_project_policy(tmp.path(), "follow-up:\n  mode: propose\n");
        replay_post_land_operation(tmp.path(), &operation.operation_id).unwrap();

        // The descendant keeps the execute-mode decision frozen on first run.
        let store = WorkModelStore::new(tmp.path());
        let derived = store.read_work_item(DERIVED_ID).unwrap();
        assert!(
            derived.authorization.is_execution_ready(),
            "a changed policy does not re-decide an already-promoted follow-up"
        );

        // The frozen policy is recorded in the journal.
        let journal_path = operation_dir(tmp.path(), &operation.operation_id).join("journal.json");
        let journal: PostLandJournal =
            serde_json::from_slice(&fs::read(&journal_path).unwrap()).unwrap();
        let frozen = journal.follow_ups[0].resolved_policy.as_ref().unwrap();
        assert_eq!(frozen.mode, "execute");
    }
}
