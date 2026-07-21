//! Record and replay landed follow-up batches into durable local effects.
//!
//! This module owns the portable handoff and operation schemas, the resumable
//! per-operation journal, corrective validation and promotion, and completion
//! evidence that cleanup uses to retain or release an origin.

use anyhow::{Context, Result, bail};
use rustix::fs::{FlockOperation, flock};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::path::{Component, Path, PathBuf};

use crate::atomic_write::atomic_write;
use crate::config::{self, CorrectionSource, FrozenFollowUpPolicy};
use crate::observations::{self, LEARNER_FOLLOW_UP_KIND, ObservationFrontmatter};
use crate::work_model::{
    CorrectiveAuditContext, CorrectiveAuthorityReference, CorrectiveContext,
    CorrectiveEvidenceReference, DerivedProvenance, ExecutionAuthority, WorkItem, WorkLineage,
    WorkModelStore, WorkModelStorageError,
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
    /// Structured project-relative files the corrective Work may change. The
    /// host uses these paths to resolve applicable `AGENTS.md` authority.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub target_paths: Vec<String>,
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
    classify_follow_up_at_revision(project_root, follow_up, "HEAD")
}

fn classify_follow_up_at_revision(
    project_root: &Path,
    follow_up: &FollowUpDraftV1,
    revision: &str,
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
    if follow_up.target_paths.is_empty()
        || follow_up.target_paths.iter().any(|target| {
            let path = Path::new(target);
            !path_is_canonical_relative(path)
        })
    {
        return ObservationOnly {
            reason: "corrective follow-up has no complete normalized target path set".to_string(),
        };
    }
    let Some(authority) = follow_up.authority.as_ref() else {
        return ObservationOnly {
            reason: "corrective follow-up cites no authority".to_string(),
        };
    };
    if let Err(reason) = verify_authority(
        project_root,
        revision,
        authority,
        context,
        &follow_up.target_paths,
    ) {
        return ObservationOnly { reason };
    }
    FollowUpClassification::Corrective
}

/// Whether an authority locator's path lives in the trusted namespace its kind
/// requires. The path must stay confined to the project, its target must be a
/// regular file tracked in `HEAD`, and the cited text must come from that
/// committed blob rather than an untracked or working-tree-only edit.
fn authority_namespace_ok(kind: AuthorityKind, path: &str) -> bool {
    match kind {
        AuthorityKind::BehaviorStatement => path == "documentation/behaviors.md",
        AuthorityKind::AgentsInstruction => {
            Path::new(path).file_name().and_then(|name| name.to_str()) == Some("AGENTS.md")
        }
        AuthorityKind::ExpertiseEntry => path.starts_with(".fluent/expertise/"),
    }
}

fn path_is_canonical_relative(path: &Path) -> bool {
    if path.as_os_str().is_empty() || !path.is_relative() {
        return false;
    }
    let Some(raw) = path.to_str() else {
        return false;
    };
    if raw.chars().any(char::is_control) {
        return false;
    }
    let mut canonical = PathBuf::new();
    for component in path.components() {
        let Component::Normal(component) = component else {
            return false;
        };
        canonical.push(component);
    }
    canonical.as_os_str() == path.as_os_str()
}

/// Resolve an authority locator against the project tree. It must be relative,
/// live in its kind's trusted namespace, be self-consistent with its digest, and
/// its anchor text must still be present in the committed file. The corrective
/// requirement must equal that anchor so an unrelated passage cannot authorize
/// different Work. A missing file or vanished anchor is stale, so the follow-up
/// stays Observation-only.
fn verify_authority(
    project_root: &Path,
    revision: &str,
    authority: &AuthorityLocator,
    context: &CorrectiveContext,
    target_paths: &[String],
) -> Result<(), String> {
    let relative = Path::new(&authority.path);
    if !path_is_canonical_relative(relative) {
        return Err(format!(
            "authority path {:?} is not a normalized relative path",
            authority.path
        ));
    }
    if !authority_namespace_ok(authority.kind, &authority.path) {
        return Err(format!(
            "authority path {:?} is not in the {:?} trusted namespace",
            authority.path, authority.kind
        ));
    }
    if authority.anchor.trim().is_empty() {
        return Err("authority anchor is empty".to_string());
    }
    if context.requirement.trim() != authority.anchor.trim() {
        return Err("corrective requirement does not match its authority anchor".to_string());
    }
    if authority.digest != content_digest(authority.anchor.as_bytes()) {
        return Err("authority digest does not match its anchor".to_string());
    }

    if authority.kind == AuthorityKind::AgentsInstruction {
        for target in target_paths {
            let applicable = applicable_agents_path(project_root, revision, Path::new(target))?;
            if applicable.as_deref() != Some(relative) {
                return Err(format!(
                    "authority {:?} is not the applicable AGENTS.md for target {:?}",
                    authority.path, target
                ));
            }
        }
    }

    let committed = committed_regular_blob(project_root, revision, &authority.path)?;
    let anchor = authority.anchor.as_bytes();
    if !committed
        .windows(anchor.len())
        .any(|candidate| candidate == anchor)
    {
        return Err(format!(
            "committed authority {:?} does not contain the cited text",
            authority.path
        ));
    }
    Ok(())
}

fn committed_tree_entry(
    project_root: &Path,
    revision: &str,
    path: &str,
) -> Result<Option<(String, String, String)>, String> {
    let output = crate::git::run_raw(project_root, &["ls-tree", "-z", revision, "--", path])
        .map_err(|_| format!("inspect committed path {path:?} at {revision}"))?;
    if !output.status.success() {
        return Err(format!("revision {revision:?} is not available for authority validation"));
    }
    if output.stdout.is_empty() {
        return Ok(None);
    }
    let entry = output.stdout.strip_suffix(&[0]).unwrap_or(&output.stdout);
    let Some(tab) = entry.iter().position(|byte| *byte == b'\t') else {
        return Err(format!("committed path {path:?} has an invalid tree entry"));
    };
    if &entry[tab + 1..] != path.as_bytes() {
        return Err(format!("committed tree entry does not exactly match {path:?}"));
    }
    let header = String::from_utf8(entry[..tab].to_vec())
        .map_err(|_| format!("committed path {path:?} has a non-UTF-8 tree entry"))?;
    let mut fields = header.split_whitespace();
    let mode = fields.next().unwrap_or_default().to_string();
    let kind = fields.next().unwrap_or_default().to_string();
    let object = fields.next().unwrap_or_default().to_string();
    if fields.next().is_some() || object.is_empty() {
        return Err(format!("committed path {path:?} has an invalid tree entry"));
    }
    Ok(Some((mode, kind, object)))
}

fn committed_regular_blob(project_root: &Path, revision: &str, path: &str) -> Result<Vec<u8>, String> {
    let Some((mode, kind, object)) = committed_tree_entry(project_root, revision, path)? else {
        return Err(format!("authority {path:?} is not committed at {revision}"));
    };
    if kind != "blob" || (mode != "100644" && mode != "100755") {
        return Err(format!("authority {path:?} is not a committed regular file at {revision}"));
    }
    let output = crate::git::run_raw(project_root, &["cat-file", "blob", &object])
        .map_err(|_| format!("read committed authority {path:?} at {revision}"))?;
    if !output.status.success() {
        return Err(format!("read committed authority {path:?} at {revision}"));
    }
    Ok(output.stdout)
}

fn applicable_agents_path(
    project_root: &Path,
    revision: &str,
    target: &Path,
) -> Result<Option<PathBuf>, String> {
    let mut directory = target.parent().unwrap_or_else(|| Path::new(""));
    loop {
        let candidate = if directory.as_os_str().is_empty() {
            PathBuf::from("AGENTS.md")
        } else {
            directory.join("AGENTS.md")
        };
        let candidate_text = candidate
            .to_str()
            .ok_or_else(|| "AGENTS.md candidate path is not UTF-8".to_string())?;
        if let Some((mode, kind, _)) =
            committed_tree_entry(project_root, revision, candidate_text)?
        {
            if kind != "blob" || (mode != "100644" && mode != "100755") {
                return Err(format!("applicable authority {candidate_text:?} is not a regular file"));
            }
            return Ok(Some(candidate));
        }
        if directory.as_os_str().is_empty() {
            return Ok(None);
        }
        directory = directory.parent().unwrap_or_else(|| Path::new(""));
    }
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
    /// Whether an execution-ready derived Work Item still needs a regular-queue
    /// dispatch. Recorded before enqueueing so an interrupted queue write is
    /// recognized as the first incomplete stage on retry.
    #[serde(default, skip_serializing_if = "is_false")]
    pub queue_expected: bool,
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
    operation_id_for_candidate(&origin.work_item_id, &origin.merge_candidate_id)
}

/// The stable operation identity for a Work Item's landed Merge Candidate.
pub fn operation_id_for_candidate(work_item_id: &str, merge_candidate_id: &str) -> String {
    format!(
        "land-{}",
        identity_digest(&[work_item_id, merge_candidate_id])
    )
}

fn legacy_operation_id_for(origin: &PostLandOrigin) -> String {
    format!("{}-{}", origin.work_item_id, origin.merge_candidate_id)
}

fn discover_recorded_operation_id(
    project_root: &Path,
    origin: &PostLandOrigin,
) -> Result<Option<String>> {
    let current = operation_id_for(origin);
    let legacy = legacy_operation_id_for(origin);
    let mut found = Vec::new();
    for operation_id in [&current, &legacy] {
        let dir = operation_dir(project_root, operation_id);
        let operation_path = dir.join("operation.json");
        let batch_path = dir.join("batch.json");
        let journal_path = dir.join("journal.json");
        let mut has_evidence = false;
        if operation_path.exists() {
            has_evidence = true;
            let operation: PendingPostLandOperationV1 =
                serde_json::from_slice(&fs::read(&operation_path)?).with_context(|| {
                    format!("parse recorded operation {}", operation_path.display())
                })?;
            if operation.schema_version != PendingPostLandOperationV1::SCHEMA_VERSION
                || operation.operation_id != *operation_id
                || operation.origin != *origin
                || operation_identity_digest(&operation)? != operation.digest
            {
                bail!("post-land operation {operation_id} does not match its recorded V1 identity");
            }
        }
        if batch_path.exists() {
            has_evidence = true;
            let batch: NormalizedFollowUpBatchV1 =
                serde_json::from_slice(&fs::read(&batch_path)?).with_context(|| {
                    format!("parse recorded batch {}", batch_path.display())
                })?;
            if batch.schema_version != NormalizedFollowUpBatchV1::SCHEMA_VERSION
                || batch.origin != *origin
            {
                bail!("post-land batch {operation_id} does not match its landed origin");
            }
        }
        if journal_path.exists() {
            has_evidence = true;
            let journal: PostLandJournal =
                serde_json::from_slice(&fs::read(&journal_path)?).with_context(|| {
                    format!("parse recorded journal {}", journal_path.display())
                })?;
            if journal.schema_version != PostLandJournal::SCHEMA_VERSION
                || journal.operation_id != *operation_id
            {
                bail!("post-land journal {operation_id} does not match its V1 identity");
            }
        }
        if has_evidence {
            found.push(operation_id.clone());
        }
    }

    collect_effect_operation_evidence(project_root, origin, &current, &legacy, &mut found)?;
    found.sort();
    found.dedup();
    match found.as_slice() {
        [] => Ok(None),
        [operation_id] => Ok(Some(operation_id.clone())),
        _ => bail!(
            "landed origin has both legacy and current post-land operations; refusing to choose"
        ),
    }
}

fn collect_effect_operation_evidence(
    project_root: &Path,
    origin: &PostLandOrigin,
    current: &str,
    legacy: &str,
    found: &mut Vec<String>,
) -> Result<()> {
    let classify = |effect_id: &str, kind: &str| -> Result<Option<String>> {
        if effect_id.starts_with(&format!("derived-{current}-"))
            || effect_id.starts_with(&format!("followup-{current}-"))
        {
            Ok(Some(current.to_string()))
        } else if effect_id.starts_with(&format!("derived-{legacy}-"))
            || effect_id.starts_with(&format!("followup-{legacy}-"))
        {
            Ok(Some(legacy.to_string()))
        } else {
            bail!("{kind} {effect_id:?} matches the landed origin but not a known V1 identity")
        }
    };

    let store = WorkModelStore::new(project_root);
    for item in store.list_work_item_results()? {
        let item = match item {
            Ok(item) => item,
            Err(error) => {
                let Some(id) = error
                    .path()
                    .filter(|path| path.parent() == Some(store.work_items_dir().as_path()))
                    .and_then(|path| path.file_stem())
                    .and_then(|stem| stem.to_str())
                else {
                    return Err(error.into());
                };
                if let Some(operation_id) = classify(id, "malformed derived Work Item")? {
                    found.push(operation_id);
                    continue;
                }
                return Err(error.into());
            }
        };
        let Some(provenance) = item.origin.provenance() else {
            continue;
        };
        if provenance.work_item_id.as_deref() == Some(origin.work_item_id.as_str())
            && provenance.attempt_id.as_deref() == Some(origin.attempt_id.as_str())
            && provenance.merge_candidate_id.as_deref()
                == Some(origin.merge_candidate_id.as_str())
            && provenance.merged_commit.as_deref() == Some(origin.merged_commit.as_str())
            && let Some(operation_id) = classify(&item.id, "derived Work Item")?
        {
            found.push(operation_id);
        }
    }

    for directory in [
        project_root.join(".fluent/observations"),
        project_root.join(".fluent/observations/resolved"),
    ] {
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("md") {
                continue;
            }
            let content = fs::read_to_string(&path)?;
            let (frontmatter, _) = observations::split_frontmatter(&content);
            let Some(frontmatter) = frontmatter else {
                continue;
            };
            if frontmatter.kind.as_deref() == Some(LEARNER_FOLLOW_UP_KIND)
                && frontmatter.work_item_id.as_deref() == Some(origin.work_item_id.as_str())
                && frontmatter.attempt_id.as_deref() == Some(origin.attempt_id.as_str())
                && frontmatter.merge_candidate_id.as_deref()
                    == Some(origin.merge_candidate_id.as_str())
                && frontmatter.merged_commit.as_deref() == Some(origin.merged_commit.as_str())
            {
                let id = path
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .context("provenance Observation id is not UTF-8")?;
                if let Some(operation_id) = classify(id, "Observation")? {
                    found.push(operation_id);
                }
            }
        }
    }
    Ok(())
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
    let _origin_lock = lock_origin(project_root, &batch.origin)?;
    let operation_id = discover_recorded_operation_id(project_root, &batch.origin)?
        .unwrap_or_else(|| operation_id_for(&batch.origin));
    let dir = operation_dir(project_root, &operation_id);
    fs::create_dir_all(&dir)
        .with_context(|| format!("create post-land operation dir {}", dir.display()))?;

    let batch_bytes = canonical_json_bytes(batch)?;
    let batch_digest = content_digest(&batch_bytes);
    let batch_rel = batch_rel_path(&operation_id);

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

    // Recording and replay share one per-operation lock. Validate an existing
    // immutable identity before replacing any referenced bytes, so a
    // conflicting or concurrent re-record cannot corrupt the accepted batch.
    let _lock = lock_operation(&dir, &operation_id, "RECORD")?;

    let op_path = dir.join("operation.json");
    if op_path.exists() {
        let existing: PendingPostLandOperationV1 = serde_json::from_slice(&fs::read(&op_path)?)
            .with_context(|| format!("parse recorded operation {}", op_path.display()))?;
        if operation_identity_digest(&existing)? != existing.digest || existing != operation {
            bail!(
                "post-land operation {operation_id} was already recorded with a conflicting \
                 identity"
            );
        }
        load_verified_batch(project_root, &existing.batch_ref).with_context(|| {
            format!("verify recorded operation {operation_id} batch before reuse")
        })?;
        return Ok(existing);
    }

    let batch_path = project_root.join(&operation.batch_ref.path);
    if batch_path.exists() {
        let existing = fs::read(&batch_path)?;
        if content_digest(&existing) != operation.batch_ref.digest || existing != batch_bytes {
            bail!(
                "post-land operation {operation_id} has conflicting durable batch evidence"
            );
        }
    }

    atomic_write(&batch_path, &batch_bytes)
        .with_context(|| format!("write normalized batch {}", operation.batch_ref.path))?;

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
    // Serialize the full read/validate/replay snapshot with recording and other
    // processors of this operation.
    let _lock = lock_operation(&dir, operation_id, "REPLAY")?;
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
    let store = WorkModelStore::new(project_root);

    let mut totals = ReplayTotals::default();
    for follow_up in &batch.follow_ups {
        process_one_follow_up(
            &store,
            project_root,
            &operation,
            batch.source,
            &batch.learning_summary,
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
    learning_summary: &str,
    follow_up: &FollowUpDraftV1,
    journal_path: &Path,
    journal: &mut PostLandJournal,
    totals: &mut ReplayTotals,
) -> Result<()> {
    let observation_id = observation_id_for_operation(operation, &follow_up.id);

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
        let corrective = classify_follow_up_at_revision(
            project_root,
            follow_up,
            &operation.origin.merged_commit,
        )
        .is_corrective();
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
    let derived_id = derived_work_item_id_for_operation(operation, &follow_up.id);
    if journal.follow_ups[index]
        .derived_work_item_id
        .as_deref()
        .is_some_and(|recorded| recorded != derived_id)
    {
        bail!("corrective receipt names a conflicting derived Work identity");
    }
    let created = ensure_derived_work(
        store,
        project_root,
        &operation.origin,
        &observation_id,
        &derived_id,
        source,
        learning_summary,
        follow_up,
        &policy,
    )?;
    if created {
        totals.work_items_created += 1;
    }
    if journal.follow_ups[index].derived_work_item_id.is_none() {
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
            // Record that a dispatch is expected before writing it, so an
            // interrupted queue write is recognized as the incomplete stage.
            if !journal.follow_ups[index].queue_expected {
                journal.follow_ups[index].queue_expected = true;
                write_journal(journal_path, journal)?;
            }
            let dispatch = crate::queue::ensure_dispatch(
                project_root,
                &derived_id,
                &intent.origin_operation_id,
                intent.priority,
            )?;
            if dispatch == crate::queue::EnsureDispatchOutcome::Created {
                totals.work_items_queued += 1;
            }
            journal.follow_ups[index].queued = true;
            write_journal(journal_path, journal)?;
        }
    }

    Ok(())
}

/// The first stage of a recorded post-land operation that has not completed, in
/// follow-up order: `observation`, `classify`, `policy`, `work`, or `queue`.
/// Returns `None` when every follow-up's required stages are complete, and is
/// used to name where a partial-failure retry resumes.
pub fn first_incomplete_stage(project_root: &Path, operation_id: &str) -> Option<String> {
    let dir = operation_dir(project_root, operation_id);
    let operation: PendingPostLandOperationV1 =
        serde_json::from_slice(&fs::read(dir.join("operation.json")).ok()?).ok()?;
    let batch = load_verified_batch(project_root, &operation.batch_ref).ok()?;
    let journal = read_journal(&dir.join("journal.json"), operation_id).ok()?;
    for follow_up in &batch.follow_ups {
        let Some(receipt) = journal
            .follow_ups
            .iter()
            .find(|receipt| receipt.follow_up_id == follow_up.id)
        else {
            return Some("observation".to_string());
        };
        if receipt.corrective.is_none() {
            return Some("classify".to_string());
        }
        if receipt.corrective == Some(true) {
            if receipt.resolved_policy.is_none() {
                return Some("policy".to_string());
            }
            if receipt.derived_work_item_id.is_none() {
                return Some("work".to_string());
            }
            if receipt.queue_expected && !receipt.queued {
                return Some("queue".to_string());
            }
        }
    }
    None
}

/// Resolve the persisted V1 identity for a landed origin before reporting its
/// first incomplete stage, so legacy operations never fall back to the hashed
/// identity during recovery diagnostics.
pub fn first_incomplete_stage_for_origin(
    project_root: &Path,
    origin: &PostLandOrigin,
) -> Option<String> {
    let _origin_lock = lock_origin(project_root, origin).ok()?;
    let operation_id = discover_recorded_operation_id(project_root, origin).ok()??;
    first_incomplete_stage(project_root, &operation_id)
}

/// Prove that a landed origin's operation, journal, receipts, and durable
/// effects are complete. Missing or incomplete evidence returns `false`;
/// malformed or conflicting evidence returns an error so cleanup fails closed.
pub fn post_land_operation_complete(
    project_root: &Path,
    expected_origin: &PostLandOrigin,
) -> Result<bool> {
    macro_rules! incomplete {
        ($reason:literal) => {{
            let _ = $reason;
            return Ok(false);
        }};
    }
    let _origin_lock = lock_origin(project_root, expected_origin)?;
    let Some(operation_id) = discover_recorded_operation_id(project_root, expected_origin)? else {
        incomplete!("missing operation");
    };
    let dir = operation_dir(project_root, &operation_id);
    let _lock = lock_operation(&dir, &operation_id, "CLEANUP")?;
    let operation: PendingPostLandOperationV1 = serde_json::from_slice(
        &fs::read(dir.join("operation.json"))?,
    )?;
    if operation.schema_version != PendingPostLandOperationV1::SCHEMA_VERSION
        || operation.operation_id != operation_id
        || operation.origin != *expected_origin
        || operation_identity_digest(&operation)? != operation.digest
    {
        bail!("post-land operation identity or digest does not match its landed origin");
    }
    let batch = load_verified_batch(project_root, &operation.batch_ref)?;
    if batch.schema_version != NormalizedFollowUpBatchV1::SCHEMA_VERSION
        || batch.origin != operation.origin
    {
        bail!("normalized batch does not match its post-land operation");
    }
    let journal: PostLandJournal = serde_json::from_slice(
        &fs::read(dir.join("journal.json"))?,
    )?;
    if journal.schema_version != PostLandJournal::SCHEMA_VERSION
        || journal.operation_id != operation_id
        || !journal.completed
        || journal.follow_ups.len() != batch.follow_ups.len()
    {
        incomplete!("journal header");
    }

    let store = WorkModelStore::new(project_root);
    let mut seen = std::collections::HashSet::new();
    for follow_up in &batch.follow_ups {
        let Some(receipt) = journal
            .follow_ups
            .iter()
            .find(|receipt| receipt.follow_up_id == follow_up.id)
        else {
            incomplete!("missing receipt");
        };
        if !seen.insert(&receipt.follow_up_id) {
            bail!("post-land journal repeats a follow-up receipt");
        }
        let observation_id = observation_id_for_operation(&operation, &follow_up.id);
        if receipt.observation_id != observation_id
            || !observations::provenance_observation_exists(
                project_root,
                &observation_id,
                &follow_up_frontmatter(expected_origin, follow_up),
            )?
            || receipt.corrective.is_none()
        {
            incomplete!("observation or classification");
        }
        let expected_corrective = classify_follow_up_at_revision(
            project_root,
            follow_up,
            &operation.origin.merged_commit,
        )
        .is_corrective();
        if receipt.corrective != Some(expected_corrective) {
            bail!("post-land receipt classification does not match the validated batch");
        }
        if !expected_corrective {
            if receipt.resolved_policy.is_some()
                || receipt.derived_work_item_id.is_some()
                || receipt.queue_expected
                || receipt.queued
            {
                bail!("non-corrective post-land receipt claims corrective effects");
            }
        } else {
            let expected_work_id = derived_work_item_id_for_operation(&operation, &follow_up.id);
            let Some(policy) = receipt.resolved_policy.as_ref() else {
                incomplete!("corrective policy");
            };
            if policy.mode != "propose" && policy.mode != "execute" {
                bail!("corrective post-land receipt has an invalid frozen policy mode");
            }
            if receipt.derived_work_item_id.as_deref() != Some(&expected_work_id) {
                incomplete!("corrective receipt");
            }
            let work_item = match store.read_work_item(&expected_work_id) {
                Ok(item) => item,
                Err(WorkModelStorageError::ReadFile { source, .. })
                    if source.kind() == std::io::ErrorKind::NotFound => incomplete!("missing work"),
                Err(error) => return Err(error.into()),
            };
            let origin_item = store.read_work_item(&expected_origin.work_item_id)?;
            let root_id = origin_item
                .lineage
                .root_id(&origin_item.id)
                .to_string();
            let expected_work = expected_derived_work(
                expected_origin,
                &observation_id,
                &expected_work_id,
                batch.source,
                &batch.learning_summary,
                follow_up,
                policy,
                &root_id,
                None,
            )?;
            reconcile_reusable_derived_work(&store, &work_item, &expected_work)?;
            let expected_provenance = DerivedProvenance {
                observation_id: Some(observation_id),
                work_item_id: Some(expected_origin.work_item_id.clone()),
                attempt_id: Some(expected_origin.attempt_id.clone()),
                merge_candidate_id: Some(expected_origin.merge_candidate_id.clone()),
                merged_commit: Some(expected_origin.merged_commit.clone()),
            };
            if work_item.origin.provenance() != Some(&expected_provenance) {
                incomplete!("work provenance");
            }
            let Some(enqueue_intent) = work_item.pending_enqueue.as_ref() else {
                incomplete!("missing enqueue intent");
            };
            if enqueue_intent.origin_operation_id != expected_work_id
                || enqueue_intent.priority != policy.priority
            {
                bail!("derived Work enqueue intent does not match its frozen policy");
            }
            let queue_required = work_item.authorization.is_execution_ready();
            if receipt.queue_expected != queue_required || receipt.queued != queue_required {
                incomplete!("queue receipt");
            }
            if queue_required {
                let Some(ledger) = crate::queue::read_ledger(project_root, &expected_work_id)? else {
                    incomplete!("missing queue ledger");
                };
                if !ledger.dispatches.iter().any(|dispatch| {
                    dispatch.origin_operation_id.as_deref()
                        == Some(enqueue_intent.origin_operation_id.as_str())
                }) {
                    incomplete!("queue origin");
                }
            }
        }
    }
    Ok(true)
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

/// Materialize a merged Attempt's successful learner handoff into the local
/// backlog under the land-gated, idempotent rules. Both the local land hook and
/// a recovered post-land Learner retry call this, so a handoff that lands late is
/// processed the same way as one processed at land time. A failed, absent, or
/// handoff-less learner run has nothing to process.
pub fn materialize_learner_handoff(
    project_root: &Path,
    work_item_id: &str,
    attempt: &crate::work_model::Attempt,
    merge_candidate_id: &str,
    merged_commit: &str,
) -> Result<()> {
    let Some(learning) = attempt.learning.as_ref() else {
        return Ok(());
    };
    if !learning.is_succeeded() {
        return Ok(());
    }
    let Some(handoff_ref) = learning.handoff.as_ref() else {
        return Ok(());
    };

    let handoff = crate::learner::load_verified_handoff(project_root, handoff_ref)?;
    let origin = PostLandOrigin {
        work_item_id: work_item_id.to_string(),
        attempt_id: attempt.id.clone(),
        merge_candidate_id: merge_candidate_id.to_string(),
        merged_commit: merged_commit.to_string(),
    };
    let batch = NormalizedFollowUpBatchV1::from_learner_handoff(&handoff, origin)?;
    process_landed_batch(project_root, &batch, None)?;
    Ok(())
}

fn observation_id_for(operation_id: &str, follow_up_id: &str) -> String {
    format!(
        "followup-{operation_id}-{}",
        identity_digest(&[follow_up_id])
    )
}

fn observation_id_for_operation(
    operation: &PendingPostLandOperationV1,
    follow_up_id: &str,
) -> String {
    if operation.operation_id == legacy_operation_id_for(&operation.origin) {
        format!(
            "followup-{}-{}",
            operation.operation_id,
            legacy_sanitize_component(follow_up_id)
        )
    } else {
        observation_id_for(&operation.operation_id, follow_up_id)
    }
}

/// The deterministic Work Item id a corrective follow-up promotes into, so a
/// replay reuses the same Work rather than deriving a duplicate.
fn derived_work_item_id(operation_id: &str, follow_up_id: &str) -> String {
    format!(
        "derived-{operation_id}-{}",
        identity_digest(&[follow_up_id])
    )
}

fn derived_work_item_id_for_operation(
    operation: &PendingPostLandOperationV1,
    follow_up_id: &str,
) -> String {
    if operation.operation_id == legacy_operation_id_for(&operation.origin) {
        format!(
            "derived-{}-{}",
            operation.operation_id,
            legacy_sanitize_component(follow_up_id)
        )
    } else {
        derived_work_item_id(&operation.operation_id, follow_up_id)
    }
}

fn legacy_sanitize_component(raw: &str) -> String {
    let mut component: String = raw
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '-'
            }
        })
        .collect();
    if component.is_empty() {
        component.push_str("unnamed");
    }
    component
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
        if journal.follow_ups[index].observation_id != observation_id {
            bail!("follow-up {follow_up_id:?} receipt names a conflicting Observation identity");
        }
        return Ok(index);
    }
    journal.follow_ups.push(FollowUpReceipt {
        follow_up_id: follow_up_id.to_string(),
        observation_id: observation_id.to_string(),
        corrective: None,
        resolved_policy: None,
        derived_work_item_id: None,
        queue_expected: false,
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
#[allow(clippy::too_many_arguments)]
fn expected_derived_work(
    origin: &PostLandOrigin,
    observation_id: &str,
    derived_id: &str,
    source: FollowUpSource,
    learning_summary: &str,
    follow_up: &FollowUpDraftV1,
    policy: &FrozenFollowUpPolicy,
    root_id: &str,
    ready_authority: Option<ExecutionAuthority>,
) -> Result<WorkItem> {
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
    let lineage = WorkLineage::descendant_of(root_id.to_string(), Some(policy.descendant_limit));
    let mut item = WorkItem::derived_corrective(
        derived_id.to_string(),
        follow_up.summary.trim().to_string(),
        provenance,
        context,
        lineage,
        ready_authority,
    )?;
    item.set_enqueue_intent(policy.priority, derived_id.to_string());
    let authority = follow_up
        .authority
        .as_ref()
        .context("corrective follow-up lost its trusted authority before Work creation")?;
    item.corrective_audit = Some(CorrectiveAuditContext {
        follow_up_id: follow_up.id.clone(),
        source: match source {
            FollowUpSource::Learner => "learner",
            FollowUpSource::PostMerge => "post-merge",
        }
        .to_string(),
        summary: follow_up.summary.clone(),
        learning_summary: learning_summary.to_string(),
        expected_result: follow_up.expected_result.clone(),
        target_paths: follow_up.target_paths.clone(),
        unresolved_decisions: follow_up.unresolved_decisions.clone(),
        authority: CorrectiveAuthorityReference {
            kind: match authority.kind {
                AuthorityKind::BehaviorStatement => "behavior-statement",
                AuthorityKind::AgentsInstruction => "agents-instruction",
                AuthorityKind::ExpertiseEntry => "expertise-entry",
            }
            .to_string(),
            path: authority.path.clone(),
            anchor: authority.anchor.clone(),
            digest: authority.digest.clone(),
        },
        evidence: follow_up
            .evidence
            .iter()
            .map(|item| CorrectiveEvidenceReference {
                path: item.path.clone(),
                digest: item.digest.clone(),
            })
            .collect(),
    });
    Ok(item)
}

fn ensure_derived_work(
    store: &WorkModelStore,
    project_root: &Path,
    origin: &PostLandOrigin,
    observation_id: &str,
    derived_id: &str,
    source: FollowUpSource,
    learning_summary: &str,
    follow_up: &FollowUpDraftV1,
    policy: &FrozenFollowUpPolicy,
) -> Result<bool> {
    // The derived Work joins its originating Work Item's lineage root.
    let origin_item = store.read_work_item(&origin.work_item_id)?;
    let root_id = origin_item.lineage.root_id(&origin_item.id).to_string();

    // Distinct operations can target the same root lineage. Serialize the
    // count/authorize/create decision across them so every contender observes
    // the latest durable charges. The operation lock is already held; release
    // this root lock before the later queue stage.
    let _lineage_lock = crate::lineage_lock::acquire_automatic(project_root, &root_id)
        .with_context(|| format!("lock corrective Work lineage {root_id:?}"))?;

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

    let item = expected_derived_work(
        origin,
        observation_id,
        derived_id,
        source,
        learning_summary,
        follow_up,
        policy,
        &root_id,
        ready_authority,
    )?;

    // Recheck only after computing the complete expected identity under the
    // lineage lock. Reuse requires every immutable intake decision to match;
    // a merely valid Work Item at this deterministic id is unrelated state.
    match store.read_work_item(derived_id) {
        Ok(existing) => {
            reconcile_reusable_derived_work(store, &existing, &item)?;
            return Ok(false);
        }
        Err(WorkModelStorageError::ReadFile { source, .. })
            if source.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    store.create_work_item(&item)?;
    Ok(true)
}

fn reconcile_reusable_derived_work(
    store: &WorkModelStore,
    existing: &WorkItem,
    expected: &WorkItem,
) -> Result<()> {
    if existing.id != expected.id
        || existing.title != expected.title
        || existing.origin != expected.origin
        || existing.corrective_context != expected.corrective_context
        || existing
            .corrective_audit
            .as_ref()
            .is_some_and(|audit| Some(audit) != expected.corrective_audit.as_ref())
        || existing.lineage.root_id != expected.lineage.root_id
        || existing.lineage.descendant_limit != expected.lineage.descendant_limit
        || existing.pending_enqueue != expected.pending_enqueue
    {
        bail!(
            "conflicting derived Work Item {} does not match corrective provenance, context, \
             lineage identity, and enqueue intent",
            expected.id
        );
    }
    if existing.corrective_audit.is_none() {
        let mut reconciled = existing.clone();
        reconciled.corrective_audit = expected.corrective_audit.clone();
        store.write_work_item(&reconciled)?;
    }
    Ok(())
}

/// Authorize one proposed derived Work Item under human authority and reconcile
/// its durable queue intent. Human and automatic charging share the same root
/// lineage lock, while the Work lock protects the item-level transition.
pub fn authorize_derived_work_item(
    project_root: &Path,
    store: &WorkModelStore,
    id: &str,
) -> Result<()> {
    let initial = match store.read_work_item(id) {
        Ok(item) => item,
        Err(WorkModelStorageError::ReadFile { source, .. })
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            bail!("Work Item {id:?} not found");
        }
        Err(error) => return Err(error.into()),
    };
    let root_id = initial.lineage.root_id(&initial.id).to_string();
    let lineage_lock = crate::lineage_lock::acquire_human(project_root, &root_id)
        .with_context(|| format!("acquire lineage lock for {root_id:?}"))?;
    let work_lock_path = project_root
        .join(".fluent/work/locks")
        .join(id)
        .join("authorize.lock");
    let work_lock = crate::lease::acquire(&work_lock_path)
        .with_context(|| format!("acquire authorization lock for Work Item {id:?}"))?;

    let mut item = match store.read_work_item(id) {
        Ok(item) => item,
        Err(WorkModelStorageError::ReadFile { source, .. })
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            bail!("Work Item {id:?} not found");
        }
        Err(error) => return Err(error.into()),
    };
    item.ensure_not_abandoned()?;
    if item.authorization.is_proposed() {
        item.authorize_execution(ExecutionAuthority::Human)?;
    }
    if item.pending_enqueue.is_none() {
        let priority = config::resolve_follow_up_policy(project_root)
            .map(|policy| policy.learner_priority.value)
            .unwrap_or(0);
        item.set_enqueue_intent(priority, format!("authorize-{id}"));
    }
    let intent = item
        .pending_enqueue
        .clone()
        .expect("enqueue intent was just ensured");
    store.write_work_item(&item)?;
    drop(work_lock);
    drop(lineage_lock);

    crate::queue::ensure_dispatch(
        project_root,
        id,
        &intent.origin_operation_id,
        intent.priority,
    )?;
    Ok(())
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
fn lock_origin(project_root: &Path, origin: &PostLandOrigin) -> Result<File> {
    let directory = follow_ups_root(project_root).join(".locks");
    fs::create_dir_all(&directory)?;
    let path = directory.join(format!("{}.lock", operation_id_for(origin)));
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .open(&path)
        .with_context(|| format!("open landed-origin lock {}", path.display()))?;
    flock(&file, FlockOperation::LockExclusive)
        .map_err(std::io::Error::from)
        .with_context(|| format!("lock landed origin {}", path.display()))?;
    Ok(file)
}

fn lock_operation(dir: &Path, _operation_id: &str, _actor: &str) -> Result<File> {
    let path = dir.join("operation.lock");
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .open(&path)
        .with_context(|| format!("open operation lock {}", path.display()))?;
    match flock(&file, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => {}
        Err(error) => {
            let error = std::io::Error::from(error);
            if error.kind() != std::io::ErrorKind::WouldBlock {
                return Err(error).with_context(|| format!("lock operation {}", path.display()));
            }
            #[cfg(test)]
            crate::test_lock_probe::reach("operation", _operation_id, _actor, "BLOCKED");
            flock(&file, FlockOperation::LockExclusive)
                .map_err(std::io::Error::from)
                .with_context(|| format!("lock operation {}", path.display()))?;
        }
    }
    #[cfg(test)]
    crate::test_lock_probe::reach("operation", _operation_id, _actor, "ACQUIRED");
    Ok(file)
}

/// Hash identity components into a filename-safe value. Canonical JSON keeps
/// component boundaries and every raw byte, avoiding collisions caused by
/// delimiter concatenation or filename normalization.
fn identity_digest(parts: &[&str]) -> String {
    let bytes = serde_json::to_vec(parts).expect("string identity always serializes");
    let digest = content_digest(&bytes);
    digest
        .strip_prefix("sha256:")
        .expect("content digests use the sha256 prefix")
        .to_string()
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
            operation_id_for(&batch.origin),
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
    fn completed_operation_requires_matching_receipts_and_effects() {
        let tmp = tempfile::TempDir::new().unwrap();
        let batch = batch_with(vec![draft("fu-1", "one")]);
        let outcome = process_landed_batch(tmp.path(), &batch, None).unwrap();
        assert!(post_land_operation_complete(tmp.path(), &batch.origin).unwrap());

        let observation = tmp.path().join(format!(
            ".fluent/observations/{}.md",
            observation_id_for(&outcome.operation_id, "fu-1")
        ));
        fs::remove_file(observation).unwrap();

        assert!(!post_land_operation_complete(tmp.path(), &batch.origin).unwrap());
    }

    #[test]
    fn completed_operation_rejects_mismatched_origin() {
        let tmp = tempfile::TempDir::new().unwrap();
        let batch = batch_with(Vec::new());
        process_landed_batch(tmp.path(), &batch, None).unwrap();
        let mut wrong = batch.origin.clone();
        wrong.merged_commit = "different".to_string();

        assert!(post_land_operation_complete(tmp.path(), &wrong).is_err());
    }

    #[test]
    fn completed_operation_derives_required_queue_effect_from_work() {
        let tmp = tempfile::TempDir::new().unwrap();
        seed_root_work_item(tmp.path());
        write_project_policy(tmp.path(), "follow-up:\n  mode: execute\n");
        let authority = fresh_expertise_authority(tmp.path());
        let batch = corrective_batch(tmp.path(), FollowUpSource::Learner, Some(authority));
        let outcome = process_landed_batch(tmp.path(), &batch, None).unwrap();
        let journal_path = operation_dir(tmp.path(), &outcome.operation_id).join("journal.json");
        let mut journal = read_journal(&journal_path, &outcome.operation_id).unwrap();
        journal.follow_ups[0].queue_expected = false;
        journal.follow_ups[0].queued = false;
        write_journal(&journal_path, &journal).unwrap();

        assert!(
            !post_land_operation_complete(tmp.path(), &batch.origin).unwrap(),
            "an execution-ready Work with enqueue intent requires its dispatch regardless of receipt claims"
        );
    }

    #[test]
    fn completed_non_corrective_receipt_rejects_impossible_effects() {
        let tmp = tempfile::TempDir::new().unwrap();
        let batch = batch_with(vec![draft("fu-1", "one")]);
        let outcome = process_landed_batch(tmp.path(), &batch, None).unwrap();
        let journal_path = operation_dir(tmp.path(), &outcome.operation_id).join("journal.json");
        let mut journal = read_journal(&journal_path, &outcome.operation_id).unwrap();
        journal.follow_ups[0].derived_work_item_id = Some("impossible-work".to_string());
        journal.follow_ups[0].queue_expected = true;
        journal.follow_ups[0].queued = true;
        write_journal(&journal_path, &journal).unwrap();

        assert!(post_land_operation_complete(tmp.path(), &batch.origin).is_err());
    }

    #[test]
    fn conflicting_operation_rerecord_preserves_original_batch() {
        let tmp = tempfile::TempDir::new().unwrap();
        let original = batch_with(vec![draft("fu-1", "original")]);
        let operation = record_post_land_operation(tmp.path(), &original, None).unwrap();
        let batch_path = tmp.path().join(&operation.batch_ref.path);
        let original_bytes = fs::read(&batch_path).unwrap();

        let conflicting = batch_with(vec![draft("fu-1", "conflicting replacement")]);
        let error = record_post_land_operation(tmp.path(), &conflicting, None).unwrap_err();

        assert!(error.to_string().contains("conflicting identity"));
        assert_eq!(
            fs::read(&batch_path).unwrap(),
            original_bytes,
            "a rejected re-record cannot replace the accepted batch"
        );
        assert_eq!(
            load_verified_batch(tmp.path(), &operation.batch_ref).unwrap(),
            original
        );
    }

    #[test]
    fn replay_and_conflicting_record_serialize_at_the_operation_boundary() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut original = batch_with(vec![draft("fu-1", "accepted")]);
        original.origin.work_item_id = "operation-lock-race-root".to_string();
        let operation = record_post_land_operation(tmp.path(), &original, None).unwrap();
        let batch_path = tmp.path().join(&operation.batch_ref.path);
        let accepted_bytes = fs::read(&batch_path).unwrap();
        let mut conflicting = original.clone();
        conflicting.follow_ups[0].summary = "conflicting".to_string();

        std::thread::scope(|scope| {
            let probe = crate::test_lock_probe::ScopedLockProbe::install(
                "operation",
                &operation.operation_id,
                Some(("REPLAY", "ACQUIRED")),
            );
            let replay = scope.spawn(|| replay_post_land_operation(tmp.path(), &operation.operation_id));
            assert!(probe.wait_for("REPLAY", "ACQUIRED"));
            let record = scope.spawn(|| record_post_land_operation(tmp.path(), &conflicting, None));
            assert!(probe.wait_for("RECORD", "BLOCKED"));
            assert_eq!(fs::read(&batch_path).unwrap(), accepted_bytes);
            probe.release();
            replay.join().unwrap().unwrap();
            let error = record.join().unwrap().unwrap_err();
            assert!(error.to_string().contains("conflicting identity"));
        });
        assert_eq!(fs::read(batch_path).unwrap(), accepted_bytes);
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

        let operation_id = operation_id_for(&batch.origin);
        let content = fs::read_to_string(
            obs_dir.join(format!("{}.md", observation_id_for(&operation_id, "fu-1"))),
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
        let mut context = corrective_context_sample();
        if let Some(authority) = authority.as_ref() {
            context.requirement = authority.anchor.clone();
        }
        FollowUpDraftV1 {
            id: "fu-1".to_string(),
            summary: "Restore the retry cap".to_string(),
            corrective: true,
            corrective_context: Some(context),
            target_paths: vec!["src/retry.rs".to_string()],
            expected_result: "The retry cap is enforced again".to_string(),
            unresolved_decisions: Vec::new(),
            authority,
            evidence: Vec::new(),
        }
    }

    fn init_authority_repo(root: &Path) {
        if root.join(".git").exists() {
            return;
        }
        crate::git::run(root, &["init", "-q"], "initialize authority test repository")
            .unwrap();
        crate::git::run(
            root,
            &["config", "user.email", "test@example.com"],
            "configure authority test email",
        )
        .unwrap();
        crate::git::run(
            root,
            &["config", "user.name", "Test"],
            "configure authority test name",
        )
        .unwrap();
    }

    /// Write and commit an authoritative file so a locator over it resolves at
    /// the same revision the production gate inspects.
    fn write_authority(root: &Path, rel: &str, contents: &str) {
        init_authority_repo(root);
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
        crate::git::run(root, &["add", "--", rel], "stage test authority").unwrap();
        crate::git::run(
            root,
            &["commit", "-q", "-m", "Add test authority"],
            "commit test authority",
        )
        .unwrap();
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

    /// Extract the fenced ```json block documenting a complete corrective
    /// follow-up — the one marked `"corrective": true` — from a prompt's
    /// Markdown body.
    fn corrective_example_from_prompt(prompt: &str) -> String {
        for block in prompt.split("```json").skip(1) {
            let Some(body) = block.split("```").next() else {
                continue;
            };
            if body.contains("\"corrective\": true") {
                return body.trim().to_string();
            }
        }
        panic!("learner prompt has no corrective follow-up example");
    }

    #[test]
    fn learner_prompt_corrective_example_passes_the_gate() {
        // The prompt a real Learner reads must describe every field the
        // corrective gate requires. Parse the prompt's own corrective example
        // and run it through the production gate so the two cannot drift: if the
        // prompt omitted a field the gate needs, the example would downgrade to
        // Observation-only and this test would fail.
        let prompt = crate::content::bundled_content("prompts/learner-user.md")
            .expect("learner-user prompt is bundled");
        let example = corrective_example_from_prompt(&prompt);
        let follow_up: FollowUpDraftV1 = serde_json::from_str(&example)
            .expect("prompt corrective example is a valid FollowUpDraftV1");

        // Every gate-required field is present and well formed straight from the
        // prompt, without the test supplying any of them.
        assert!(follow_up.corrective);
        assert!(!follow_up.expected_result.trim().is_empty());
        assert!(follow_up.unresolved_decisions.is_empty());
        assert!(follow_up.corrective_context.is_some());
        let authority = follow_up
            .authority
            .as_ref()
            .expect("prompt example cites an authority");
        // The example digest is self-consistent, so a Learner copying the shape
        // and recomputing over its own anchor produces a matching locator.
        assert_eq!(authority.digest, content_digest(authority.anchor.as_bytes()));

        // Materialize the committed authority the example points at and confirm
        // the production gate classifies the prompt's example as corrective.
        let tmp = tempfile::TempDir::new().unwrap();
        write_authority(
            tmp.path(),
            &authority.path,
            &format!("# Retry cap\n\n{}\n", authority.anchor),
        );
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
    fn gate_rejects_noncanonical_authority_path_matrix() {
        let tmp = tempfile::TempDir::new().unwrap();
        let anchor = "Retries stop after the configured cap";
        let context = corrective_context_sample();
        for authority_path in [
            "",
            "/.fluent/expertise/retry.md",
            ".",
            ".fluent/expertise/./retry.md",
            ".fluent/expertise/../expertise/retry.md",
            ".fluent//expertise/retry.md",
            ".fluent/expertise/retry.md/",
            ".fluent/expertise/\0retry.md",
            ".fluent/expertise/retry\n.md",
            ".fluent/expertise/retry\u{7f}.md",
        ] {
            let authority = locator(
                AuthorityKind::ExpertiseEntry,
                authority_path,
                anchor,
            );
            let reason = verify_authority(
                tmp.path(),
                "HEAD",
                &authority,
                &context,
                &["src/retry.rs".to_string()],
            )
            .unwrap_err();
            assert!(
                reason.contains("not a normalized relative path"),
                "non-canonical authority path {authority_path:?} reached a later boundary: {reason}"
            );
        }
        for control in (0..=0x1f).chain(std::iter::once(0x7f)) {
            let control = char::from_u32(control).unwrap();
            let authority_path = format!(".fluent/expertise/retry{control}.md");
            let authority = locator(AuthorityKind::ExpertiseEntry, &authority_path, anchor);
            let reason = verify_authority(
                tmp.path(),
                "HEAD",
                &authority,
                &context,
                &["src/retry.rs".to_string()],
            )
            .unwrap_err();
            assert!(reason.contains("not a normalized relative path"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn gate_rejects_authority_symlink_escape() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::TempDir::new().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        let anchor = "Retries stop after the configured cap";
        fs::write(outside.path(), format!("{anchor}\n")).unwrap();
        init_authority_repo(tmp.path());
        let authority_dir = tmp.path().join(".fluent/expertise");
        fs::create_dir_all(&authority_dir).unwrap();
        symlink(outside.path(), authority_dir.join("retry.md")).unwrap();
        crate::git::run(
            tmp.path(),
            &["add", "--", ".fluent/expertise/retry.md"],
            "stage escaped authority symlink",
        )
        .unwrap();
        crate::git::run(
            tmp.path(),
            &["commit", "-q", "-m", "Add authority symlink"],
            "commit escaped authority symlink",
        )
        .unwrap();

        let follow_up = corrective_follow_up(Some(locator(
            AuthorityKind::ExpertiseEntry,
            ".fluent/expertise/retry.md",
            anchor,
        )));
        assert!(!classify_follow_up(tmp.path(), &follow_up).is_corrective());
    }

    #[test]
    fn gate_rejects_untracked_or_worktree_only_authority() {
        let tmp = tempfile::TempDir::new().unwrap();
        init_authority_repo(tmp.path());
        fs::write(tmp.path().join("README.md"), "project\n").unwrap();
        crate::git::run(tmp.path(), &["add", "README.md"], "stage repository seed").unwrap();
        crate::git::run(
            tmp.path(),
            &["commit", "-q", "-m", "Seed repository"],
            "commit repository seed",
        )
        .unwrap();
        let anchor = "Retries stop after the configured cap";
        let path = tmp.path().join(".fluent/expertise/retry.md");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, format!("{anchor}\n")).unwrap();
        let untracked = corrective_follow_up(Some(locator(
            AuthorityKind::ExpertiseEntry,
            ".fluent/expertise/retry.md",
            anchor,
        )));
        assert!(!classify_follow_up(tmp.path(), &untracked).is_corrective());

        crate::git::run(
            tmp.path(),
            &["add", "--", ".fluent/expertise/retry.md"],
            "stage original authority",
        )
        .unwrap();
        crate::git::run(
            tmp.path(),
            &["commit", "-q", "-m", "Add original authority"],
            "commit original authority",
        )
        .unwrap();
        let replacement = "Retries use exponential backoff";
        fs::write(&path, format!("{replacement}\n")).unwrap();
        let worktree_only = corrective_follow_up(Some(locator(
            AuthorityKind::ExpertiseEntry,
            ".fluent/expertise/retry.md",
            replacement,
        )));
        assert!(!classify_follow_up(tmp.path(), &worktree_only).is_corrective());
    }

    #[test]
    fn gate_rejects_empty_or_unrelated_authority_anchor() {
        let tmp = tempfile::TempDir::new().unwrap();
        write_authority(
            tmp.path(),
            ".fluent/expertise/retry.md",
            "Retries stop after the configured cap\n",
        );
        let empty = corrective_follow_up(Some(locator(
            AuthorityKind::ExpertiseEntry,
            ".fluent/expertise/retry.md",
            "",
        )));
        assert!(!classify_follow_up(tmp.path(), &empty).is_corrective());

        let mut unrelated = corrective_follow_up(Some(locator(
            AuthorityKind::ExpertiseEntry,
            ".fluent/expertise/retry.md",
            "Retries stop after the configured cap",
        )));
        unrelated.corrective_context.as_mut().unwrap().requirement =
            "Use a different scheduling algorithm".to_string();
        assert!(!classify_follow_up(tmp.path(), &unrelated).is_corrective());
    }

    #[test]
    fn gate_rejects_inapplicable_nested_agents_instruction() {
        let tmp = tempfile::TempDir::new().unwrap();
        let anchor = "Always enforce the configured retry cap";
        write_authority(
            tmp.path(),
            "src/AGENTS.md",
            &format!("- {anchor}\n"),
        );
        let mut follow_up = corrective_follow_up(Some(locator(
            AuthorityKind::AgentsInstruction,
            "src/AGENTS.md",
            anchor,
        )));
        follow_up.corrective_context.as_mut().unwrap().included_scope =
            "tests/retry.rs".to_string();
        follow_up.target_paths = vec!["tests/retry.rs".to_string()];

        assert!(!classify_follow_up(tmp.path(), &follow_up).is_corrective());
    }

    #[test]
    fn gate_rejects_empty_and_lexically_aliased_targets() {
        let tmp = tempfile::TempDir::new().unwrap();
        let anchor = "Cap enforcement belongs in retry.rs";
        write_authority(
            tmp.path(),
            ".fluent/expertise/retry.md",
            &format!("{anchor}\n"),
        );
        let base = corrective_follow_up(Some(locator(
            AuthorityKind::ExpertiseEntry,
            ".fluent/expertise/retry.md",
            anchor,
        )));

        for target in [
            "",
            "/src/retry.rs",
            ".",
            "src/./retry.rs",
            "src//retry.rs",
            "src/retry.rs/",
            "src/../src/retry.rs",
            "src/\0/retry.rs",
            "src/retry\t.rs",
            "src/retry\u{7f}.rs",
        ] {
            let mut follow_up = base.clone();
            follow_up.target_paths = vec![target.to_string()];
            assert!(
                !classify_follow_up(tmp.path(), &follow_up).is_corrective(),
                "lexically non-canonical target {target:?} must be rejected"
            );
        }
        for codepoint in (0..=0x1f).chain(std::iter::once(0x7f)) {
            let control = char::from_u32(codepoint).unwrap();
            let mut follow_up = base.clone();
            follow_up.target_paths = vec![format!("src/retry{control}.rs")];
            assert!(
                !classify_follow_up(tmp.path(), &follow_up).is_corrective(),
                "control U+{codepoint:04X} must not survive target validation"
            );
        }
    }

    #[test]
    fn gate_accepts_closest_nested_agents_authority_for_all_targets() {
        let tmp = tempfile::TempDir::new().unwrap();
        let anchor = "Always enforce the configured retry cap";
        write_authority(tmp.path(), "src/AGENTS.md", &format!("- {anchor}\n"));
        let mut follow_up = corrective_follow_up(Some(locator(
            AuthorityKind::AgentsInstruction,
            "src/AGENTS.md",
            anchor,
        )));
        follow_up.corrective_context.as_mut().unwrap().requirement = anchor.to_string();
        follow_up.corrective_context.as_mut().unwrap().included_scope = "src".to_string();
        follow_up.target_paths = vec!["src/retry.rs".to_string(), "src/net/client.rs".to_string()];

        assert!(classify_follow_up(tmp.path(), &follow_up).is_corrective());
    }

    #[test]
    fn replay_pins_authority_to_the_landed_revision_in_both_directions() {
        fn head(root: &Path) -> String {
            let output = crate::git::run_raw(root, &["rev-parse", "HEAD"]).unwrap();
            String::from_utf8(output.stdout).unwrap().trim().to_string()
        }

        let accepted = tempfile::TempDir::new().unwrap();
        seed_root_work_item(accepted.path());
        let authority = fresh_expertise_authority(accepted.path());
        let landed_with_authority = head(accepted.path());
        write_authority(
            accepted.path(),
            ".fluent/expertise/retry.md",
            "replacement without the accepted anchor\n",
        );
        let mut accepted_batch = corrective_batch(accepted.path(), FollowUpSource::Learner, Some(authority));
        accepted_batch.origin.merged_commit = landed_with_authority;
        assert_eq!(
            process_landed_batch(accepted.path(), &accepted_batch, None)
                .unwrap()
                .work_items_created,
            1,
            "later HEAD drift cannot invalidate authority accepted at land"
        );

        let rejected = tempfile::TempDir::new().unwrap();
        seed_root_work_item(rejected.path());
        let authority = fresh_expertise_authority(rejected.path());
        write_authority(
            rejected.path(),
            ".fluent/expertise/retry.md",
            "landed revision has no accepted anchor\n",
        );
        let landed_without_authority = head(rejected.path());
        write_authority(
            rejected.path(),
            ".fluent/expertise/retry.md",
            "Cap enforcement belongs in retry.rs\n",
        );
        let mut rejected_batch = corrective_batch(rejected.path(), FollowUpSource::Learner, Some(authority));
        rejected_batch.origin.merged_commit = landed_without_authority;
        assert_eq!(
            process_landed_batch(rejected.path(), &rejected_batch, None)
                .unwrap()
                .work_items_created,
            0,
            "later HEAD content cannot authorize a landed revision that lacked it"
        );
    }

    #[test]
    fn agents_authority_must_be_the_closest_ancestor_for_every_target() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root_anchor = "Follow project instructions";
        let nested_anchor = "Follow retry instructions";
        write_authority(tmp.path(), "AGENTS.md", &format!("{root_anchor}\n"));
        write_authority(tmp.path(), "src/AGENTS.md", &format!("{nested_anchor}\n"));

        let mut root = corrective_follow_up(Some(locator(
            AuthorityKind::AgentsInstruction,
            "AGENTS.md",
            root_anchor,
        )));
        root.target_paths = vec!["docs/guide.md".to_string(), "src/retry.rs".to_string()];
        root.corrective_context.as_mut().unwrap().included_scope = "docs and src".to_string();
        assert!(
            !classify_follow_up(tmp.path(), &root).is_corrective(),
            "a closer nested instruction overrides root authority for one target"
        );

        let mut nested = corrective_follow_up(Some(locator(
            AuthorityKind::AgentsInstruction,
            "src/AGENTS.md",
            nested_anchor,
        )));
        nested.target_paths = vec!["src/retry.rs".to_string(), "docs/guide.md".to_string()];
        nested.corrective_context.as_mut().unwrap().included_scope = "src and docs".to_string();
        assert!(
            !classify_follow_up(tmp.path(), &nested).is_corrective(),
            "nested authority cannot authorize a target outside its tree"
        );

        root.target_paths = vec!["docs/guide.md".to_string()];
        assert!(classify_follow_up(tmp.path(), &root).is_corrective());
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

    fn corrective_batch(
        root: &Path,
        source: FollowUpSource,
        authority: Option<AuthorityLocator>,
    ) -> NormalizedFollowUpBatchV1 {
        let mut batch_origin = origin();
        if root.join(".git").exists() {
            let output = crate::git::run_raw(root, &["rev-parse", "HEAD"]).unwrap();
            batch_origin.merged_commit =
                String::from_utf8(output.stdout).unwrap().trim().to_string();
        }
        NormalizedFollowUpBatchV1 {
            schema_version: NormalizedFollowUpBatchV1::SCHEMA_VERSION,
            source,
            origin: batch_origin,
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

    const DERIVED_ID: &str = "derived-land-a1478b19201ae32c3d73895587323e1200206c0803f6469558d8b376c53c3a43-a0eebf1952dae493547552e76655314a612b44205eec110b5745ccbbf378b4eb";

    fn write_legacy_operation(
        root: &Path,
        batch: &NormalizedFollowUpBatchV1,
    ) -> PendingPostLandOperationV1 {
        let operation_id = format!(
            "{}-{}",
            batch.origin.work_item_id, batch.origin.merge_candidate_id
        );
        let dir = operation_dir(root, &operation_id);
        fs::create_dir_all(&dir).unwrap();
        let batch_bytes = canonical_json_bytes(batch).unwrap();
        let batch_ref = ArtifactRef {
            path: batch_rel_path(&operation_id),
            digest: content_digest(&batch_bytes),
        };
        let mut operation = PendingPostLandOperationV1 {
            schema_version: PendingPostLandOperationV1::SCHEMA_VERSION,
            operation_id: operation_id.clone(),
            origin: batch.origin.clone(),
            batch_ref,
            review_request: None,
            digest: String::new(),
        };
        operation.digest = operation_identity_digest(&operation).unwrap();
        fs::write(root.join(&operation.batch_ref.path), batch_bytes).unwrap();
        fs::write(
            dir.join("operation.json"),
            serde_json::to_vec_pretty(&operation).unwrap(),
        )
        .unwrap();
        write_journal(
            &dir.join("journal.json"),
            &PostLandJournal::empty(&operation_id),
        )
        .unwrap();
        operation
    }

    fn write_pre_audit_work_fixture(
        root: &Path,
        item: &WorkItem,
        authorization: serde_json::Value,
        charged: bool,
    ) {
        let fixture = serde_json::json!({
            "id": item.id,
            "title": item.title,
            "origin": item.origin,
            "authorization": authorization,
            "lineage": {
                "root_id": item.lineage.root_id,
                "charged": charged,
                "descendant_limit": item.lineage.descendant_limit,
            },
            "corrective_context": item.corrective_context,
            "pending_enqueue": item.pending_enqueue,
            "attempts": [],
            "merge_candidates": [],
        });
        assert!(fixture.get("corrective_audit").is_none());
        fs::write(
            root.join(format!(".fluent/work/items/{}.json", item.id)),
            serde_json::to_vec_pretty(&fixture).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn deterministic_ids_keep_distinct_raw_identities_distinct() {
        assert_ne!(
            operation_id_for_candidate("work-a-b", "candidate-c"),
            operation_id_for_candidate("work-a", "b-candidate-c"),
            "component boundaries must contribute to operation identity"
        );
        assert_ne!(
            observation_id_for("operation", "follow/up"),
            observation_id_for("operation", "follow-up"),
            "filename normalization must not collapse distinct follow-up ids"
        );
        assert_ne!(
            derived_work_item_id("operation", "follow/up"),
            derived_work_item_id("operation", "follow-up")
        );
    }

    #[test]
    fn unrelated_work_at_derived_identity_is_rejected() {
        let tmp = tempfile::TempDir::new().unwrap();
        seed_root_work_item(tmp.path());
        let store = WorkModelStore::new(tmp.path());
        store
            .create_work_item(&WorkItem::planned(DERIVED_ID, "Unrelated valid work"))
            .unwrap();
        let authority = fresh_expertise_authority(tmp.path());
        let batch = corrective_batch(tmp.path(), FollowUpSource::Learner, Some(authority));

        let error = process_landed_batch(tmp.path(), &batch, None).unwrap_err();

        assert!(
            error.to_string().contains("conflicting derived Work Item"),
            "unexpected error: {error:#}"
        );
        assert_eq!(store.read_work_item(DERIVED_ID).unwrap().title, "Unrelated valid work");
    }

    #[test]
    fn legacy_v1_operation_reuses_pre_hash_effect_identities() {
        let tmp = tempfile::TempDir::new().unwrap();
        seed_root_work_item(tmp.path());
        let authority = fresh_expertise_authority(tmp.path());
        let batch = corrective_batch(tmp.path(), FollowUpSource::Learner, Some(authority));
        let operation = write_legacy_operation(tmp.path(), &batch);
        let hashed_id = operation_id_for(&batch.origin);

        let outcome = process_landed_batch(tmp.path(), &batch, None).unwrap();

        assert_eq!(outcome.operation_id, operation.operation_id);
        assert!(!operation_dir(tmp.path(), &hashed_id).exists());
        let observation_id = format!("followup-{}-fu-1", operation.operation_id);
        let derived_id = format!("derived-{}-fu-1", operation.operation_id);
        let derived = WorkModelStore::new(tmp.path())
            .read_work_item(&derived_id)
            .unwrap();
        assert_eq!(
            derived
                .origin
                .provenance()
                .unwrap()
                .observation_id
                .as_deref(),
            Some(observation_id.as_str())
        );
        assert!(post_land_operation_complete(tmp.path(), &batch.origin).unwrap());

        let completed_retry = process_landed_batch(tmp.path(), &batch, None).unwrap();
        assert_eq!(completed_retry.operation_id, operation.operation_id);
        assert_eq!(completed_retry.observations_created, 0);
        assert_eq!(completed_retry.work_items_created, 0);
        assert_eq!(
            WorkModelStore::new(tmp.path())
                .list_work_items()
                .unwrap()
                .len(),
            2,
            "completed legacy replay keeps one root and one derived Work Item"
        );
    }

    #[test]
    fn legacy_discovery_recovers_from_batch_and_journal_without_operation_header() {
        let tmp = tempfile::TempDir::new().unwrap();
        seed_root_work_item(tmp.path());
        let authority = fresh_expertise_authority(tmp.path());
        let batch = corrective_batch(tmp.path(), FollowUpSource::Learner, Some(authority));
        let legacy = write_legacy_operation(tmp.path(), &batch);
        let legacy_dir = operation_dir(tmp.path(), &legacy.operation_id);
        fs::remove_file(legacy_dir.join("operation.json")).unwrap();

        let recovered = record_post_land_operation(tmp.path(), &batch, None).unwrap();

        assert_eq!(recovered.operation_id, legacy.operation_id);
        assert!(legacy_dir.join("operation.json").exists());
        assert!(!operation_dir(tmp.path(), &operation_id_for(&batch.origin)).exists());
    }

    #[test]
    fn legacy_discovery_recovers_from_materialized_effects_without_journal_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        seed_root_work_item(tmp.path());
        let authority = fresh_expertise_authority(tmp.path());
        let batch = corrective_batch(tmp.path(), FollowUpSource::Learner, Some(authority));
        let legacy = write_legacy_operation(tmp.path(), &batch);
        replay_post_land_operation(tmp.path(), &legacy.operation_id).unwrap();
        fs::remove_dir_all(operation_dir(tmp.path(), &legacy.operation_id)).unwrap();

        let recovered = record_post_land_operation(tmp.path(), &batch, None).unwrap();

        assert_eq!(recovered.operation_id, legacy.operation_id);
        assert!(!operation_dir(tmp.path(), &operation_id_for(&batch.origin)).exists());
    }

    #[test]
    fn legacy_discovery_rejects_ambiguous_partial_layouts() {
        let tmp = tempfile::TempDir::new().unwrap();
        seed_root_work_item(tmp.path());
        let authority = fresh_expertise_authority(tmp.path());
        let batch = corrective_batch(tmp.path(), FollowUpSource::Learner, Some(authority));
        write_legacy_operation(tmp.path(), &batch);
        let current = operation_id_for(&batch.origin);
        let current_dir = operation_dir(tmp.path(), &current);
        fs::create_dir_all(&current_dir).unwrap();
        write_journal(&current_dir.join("journal.json"), &PostLandJournal::empty(&current))
            .unwrap();

        let error = record_post_land_operation(tmp.path(), &batch, None)
            .unwrap_err()
            .to_string();

        assert!(error.contains("both legacy and current"));
    }

    #[test]
    fn recovery_stage_uses_discovered_legacy_identity() {
        let tmp = tempfile::TempDir::new().unwrap();
        seed_root_work_item(tmp.path());
        let authority = fresh_expertise_authority(tmp.path());
        let batch = corrective_batch(tmp.path(), FollowUpSource::Learner, Some(authority));
        let legacy = write_legacy_operation(tmp.path(), &batch);

        assert_eq!(
            first_incomplete_stage_for_origin(tmp.path(), &batch.origin).as_deref(),
            Some("observation")
        );
        assert_ne!(legacy.operation_id, operation_id_for(&batch.origin));
    }

    #[test]
    fn pre_audit_work_is_backfilled_after_work_before_receipt_crash() {
        let tmp = tempfile::TempDir::new().unwrap();
        seed_root_work_item(tmp.path());
        let authority = fresh_expertise_authority(tmp.path());
        let batch = corrective_batch(tmp.path(), FollowUpSource::Learner, Some(authority));
        let legacy = write_legacy_operation(tmp.path(), &batch);
        replay_post_land_operation(tmp.path(), &legacy.operation_id).unwrap();
        let derived_id = format!("derived-{}-fu-1", legacy.operation_id);
        let store = WorkModelStore::new(tmp.path());
        let derived = store.read_work_item(&derived_id).unwrap();
        write_pre_audit_work_fixture(
            tmp.path(),
            &derived,
            serde_json::json!({"state": "execution-ready", "authority": "human"}),
            true,
        );
        let journal_path = operation_dir(tmp.path(), &legacy.operation_id).join("journal.json");
        let mut journal = read_journal(&journal_path, &legacy.operation_id).unwrap();
        journal.completed = false;
        journal.follow_ups[0].derived_work_item_id = None;
        journal.follow_ups[0].queue_expected = false;
        journal.follow_ups[0].queued = false;
        write_journal(&journal_path, &journal).unwrap();

        let replay = replay_post_land_operation(tmp.path(), &legacy.operation_id).unwrap();

        assert_eq!(replay.work_items_created, 0);
        let reconciled = store.read_work_item(&derived_id).unwrap();
        assert!(reconciled.corrective_audit.is_some());
        assert_eq!(
            reconciled.authorization.authority(),
            Some(ExecutionAuthority::Human)
        );
        assert!(reconciled.lineage.charged);
    }

    #[test]
    fn completed_pre_audit_work_is_validated_and_backfilled_before_cleanup() {
        let tmp = tempfile::TempDir::new().unwrap();
        seed_root_work_item(tmp.path());
        let authority = fresh_expertise_authority(tmp.path());
        let batch = corrective_batch(tmp.path(), FollowUpSource::Learner, Some(authority));
        let legacy = write_legacy_operation(tmp.path(), &batch);
        replay_post_land_operation(tmp.path(), &legacy.operation_id).unwrap();
        let derived_id = format!("derived-{}-fu-1", legacy.operation_id);
        let store = WorkModelStore::new(tmp.path());
        let derived = store.read_work_item(&derived_id).unwrap();
        write_pre_audit_work_fixture(
            tmp.path(),
            &derived,
            serde_json::json!({"state": "proposed"}),
            false,
        );

        assert!(post_land_operation_complete(tmp.path(), &batch.origin).unwrap());
        assert!(
            store
                .read_work_item(&derived_id)
                .unwrap()
                .corrective_audit
                .is_some()
        );
    }

    #[test]
    fn receipt_does_not_bless_tampered_derived_work_identity() {
        for field in [
            "title",
            "context",
            "audit",
            "lineage-root",
            "lineage-limit",
            "enqueue",
        ] {
            let tmp = tempfile::TempDir::new().unwrap();
            seed_root_work_item(tmp.path());
            let authority = fresh_expertise_authority(tmp.path());
            let batch = corrective_batch(tmp.path(), FollowUpSource::Learner, Some(authority));
            let operation = record_post_land_operation(tmp.path(), &batch, None).unwrap();
            replay_post_land_operation(tmp.path(), &operation.operation_id).unwrap();
            let store = WorkModelStore::new(tmp.path());
            let mut derived = store.read_work_item(DERIVED_ID).unwrap();
            match field {
                "title" => derived.title = "tampered after receipt".to_string(),
                "context" => {
                    derived.corrective_context.as_mut().unwrap().objective =
                        "tampered objective".to_string()
                }
                "audit" => {
                    derived.corrective_audit.as_mut().unwrap().learning_summary =
                        "tampered learning".to_string()
                }
                "lineage-root" => derived.lineage.root_id = Some("different-root".to_string()),
                "lineage-limit" => derived.lineage.descendant_limit = Some(999),
                "enqueue" => derived.pending_enqueue.as_mut().unwrap().priority += 1,
                _ => unreachable!(),
            }
            store.write_work_item(&derived).unwrap();

            assert!(
                replay_post_land_operation(tmp.path(), &operation.operation_id).is_err(),
                "replay accepted tampered {field}"
            );
            assert!(
                post_land_operation_complete(tmp.path(), &batch.origin).is_err(),
                "completion accepted tampered {field}"
            );
        }
    }

    #[test]
    fn replay_reuses_human_authorized_work_after_receipt_loss() {
        let tmp = tempfile::TempDir::new().unwrap();
        seed_root_work_item(tmp.path());
        let authority = fresh_expertise_authority(tmp.path());
        let batch = corrective_batch(tmp.path(), FollowUpSource::Learner, Some(authority));
        let operation = record_post_land_operation(tmp.path(), &batch, None).unwrap();
        replay_post_land_operation(tmp.path(), &operation.operation_id).unwrap();

        let store = WorkModelStore::new(tmp.path());
        let mut derived = store.read_work_item(DERIVED_ID).unwrap();
        derived
            .authorize_execution(ExecutionAuthority::Human)
            .unwrap();
        store.write_work_item(&derived).unwrap();
        let journal_path = operation_dir(tmp.path(), &operation.operation_id).join("journal.json");
        let mut journal = read_journal(&journal_path, &operation.operation_id).unwrap();
        journal.completed = false;
        journal.follow_ups[0].derived_work_item_id = None;
        write_journal(&journal_path, &journal).unwrap();

        let outcome = replay_post_land_operation(tmp.path(), &operation.operation_id).unwrap();

        assert_eq!(outcome.work_items_created, 0);
        let derived = store.read_work_item(DERIVED_ID).unwrap();
        assert_eq!(
            derived.authorization.authority(),
            Some(ExecutionAuthority::Human)
        );
        assert!(derived.lineage.charged);
        assert_eq!(
            crate::queue::read_ledger(tmp.path(), DERIVED_ID)
                .unwrap()
                .unwrap()
                .dispatches
                .len(),
            1
        );
    }

    #[test]
    fn non_corrective_follow_up_stays_observation_only() {
        let tmp = tempfile::TempDir::new().unwrap();
        seed_root_work_item(tmp.path());
        // A follow-up with no authority never passes the gate.
        let batch = corrective_batch(tmp.path(), FollowUpSource::Learner, None);
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
        let batch = corrective_batch(tmp.path(), FollowUpSource::Learner, Some(authority));

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
            Some("followup-land-a1478b19201ae32c3d73895587323e1200206c0803f6469558d8b376c53c3a43-a0eebf1952dae493547552e76655314a612b44205eec110b5745ccbbf378b4eb")
        );
        assert!(
            crate::queue::read_ledger(tmp.path(), DERIVED_ID)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn derived_work_retains_complete_audit_context_without_origin_files() {
        let tmp = tempfile::TempDir::new().unwrap();
        seed_root_work_item(tmp.path());
        let authority = fresh_expertise_authority(tmp.path());
        let mut batch = corrective_batch(tmp.path(), FollowUpSource::PostMerge, Some(authority));
        batch.follow_ups[0].evidence.push(ArtifactRef {
            path: "artifacts/review.md".to_string(),
            digest: "sha256:evidence".to_string(),
        });

        process_landed_batch(tmp.path(), &batch, None).unwrap();
        fs::remove_dir_all(follow_ups_root(tmp.path())).unwrap();

        let derived = WorkModelStore::new(tmp.path()).read_work_item(DERIVED_ID).unwrap();
        let audit = derived.corrective_audit.as_ref().expect("accepted audit context");
        assert_eq!(audit.follow_up_id, "fu-1");
        assert_eq!(audit.source, "post-merge");
        assert_eq!(audit.learning_summary, "learned");
        assert_eq!(audit.expected_result, "The retry cap is enforced again");
        assert!(audit.unresolved_decisions.is_empty());
        assert_eq!(audit.authority.path, ".fluent/expertise/retry.md");
        assert_eq!(audit.authority.anchor, "Cap enforcement belongs in retry.rs");
        assert_eq!(audit.evidence[0].path, "artifacts/review.md");
        assert_eq!(audit.evidence[0].digest, "sha256:evidence");

        let instructions = derived.write_task_instructions().unwrap();
        assert!(instructions.contains("## Expected result\nThe retry cap is enforced again"));
        assert!(instructions.contains("## Trusted authority"));
        assert!(instructions.contains(".fluent/expertise/retry.md"));
        assert!(instructions.contains("artifacts/review.md (sha256:evidence)"));
    }

    #[test]
    fn execute_mode_with_budget_creates_ready_queued_descendant() {
        let tmp = tempfile::TempDir::new().unwrap();
        seed_root_work_item(tmp.path());
        write_project_policy(tmp.path(), "follow-up:\n  mode: execute\n");
        let authority = fresh_expertise_authority(tmp.path());
        let batch = corrective_batch(tmp.path(), FollowUpSource::Learner, Some(authority));

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
        let batch = corrective_batch(tmp.path(), FollowUpSource::Learner, Some(authority));
        let outcome = process_landed_batch(tmp.path(), &batch, None).unwrap();

        assert_eq!(outcome.work_items_created, 1);
        assert_eq!(outcome.work_items_queued, 0, "an exhausted budget does not enqueue");
        let derived = store.read_work_item(DERIVED_ID).unwrap();
        assert!(derived.authorization.is_proposed());
        assert!(!derived.lineage.charged);
    }

    #[test]
    fn different_operations_share_one_root_lineage_lock() {
        let tmp = tempfile::TempDir::new().unwrap();
        seed_root_work_item(tmp.path());
        write_project_policy(
            tmp.path(),
            "follow-up:\n  mode: execute\n  descendant-limit: 1\n",
        );
        let authority = fresh_expertise_authority(tmp.path());
        let first = corrective_batch(tmp.path(), FollowUpSource::Learner, Some(authority.clone()));
        let mut second = corrective_batch(tmp.path(), FollowUpSource::PostMerge, Some(authority));
        second.origin.attempt_id = "attempt-2".to_string();
        second.origin.merge_candidate_id = "attempt-2-merge-candidate".to_string();

        // Hold the root lock before either distinct operation starts. Production
        // processing must stop before count/authorize/create until it can take
        // this same lock; a per-operation lock would not block either thread.
        let root_lock = crate::lineage_lock::acquire(tmp.path(), "work-1").unwrap();
        std::thread::scope(|scope| {
            let first_run = scope.spawn(|| process_landed_batch(tmp.path(), &first, None));
            let second_run = scope.spawn(|| process_landed_batch(tmp.path(), &second, None));

            let store = WorkModelStore::new(tmp.path());
            let mut both_waiting_at_work = false;
            for _ in 0..100 {
                if first_incomplete_stage(tmp.path(), &operation_id_for(&first.origin))
                    == Some("work".to_string())
                    && first_incomplete_stage(tmp.path(), &operation_id_for(&second.origin))
                        == Some("work".to_string())
                {
                    both_waiting_at_work = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            assert!(
                both_waiting_at_work,
                "both distinct operations reach the shared lineage lock"
            );
            assert_eq!(
                store.list_work_items().unwrap().len(),
                1,
                "both operations wait at the shared root-lineage boundary"
            );

            drop(root_lock);
            first_run.join().unwrap().unwrap();
            second_run.join().unwrap().unwrap();
        });

        let descendants: Vec<WorkItem> = WorkModelStore::new(tmp.path())
            .list_work_items()
            .unwrap()
            .into_iter()
            .filter(|item| item.origin.is_derived())
            .collect();
        assert_eq!(descendants.len(), 2);
        assert_eq!(
            descendants.iter().filter(|item| item.lineage.charged).count(),
            1,
            "different operations cannot overspend the one-slot lineage"
        );
    }

    #[test]
    fn automatic_promotion_contends_with_human_authorization_on_root_lineage() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root_id = "lineage-contention-root-unique";
        let store = WorkModelStore::new(tmp.path());
        store
            .create_work_item(&WorkItem::planned(root_id, "Root work"))
            .unwrap();
        write_project_policy(
            tmp.path(),
            "follow-up:\n  mode: execute\n  descendant-limit: 1\n",
        );

        let mut human = WorkItem::derived_corrective(
            "human-descendant",
            "Human override",
            DerivedProvenance {
                work_item_id: Some(root_id.to_string()),
                ..Default::default()
            },
            corrective_context_sample(),
            WorkLineage::descendant_of(root_id, Some(1)),
            None,
        )
        .unwrap();
        human.set_enqueue_intent(0, "human-descendant");
        store.create_work_item(&human).unwrap();

        let authority = fresh_expertise_authority(tmp.path());
        let mut automatic =
            corrective_batch(tmp.path(), FollowUpSource::Learner, Some(authority));
        automatic.origin.work_item_id = root_id.to_string();
        let automatic_id = derived_work_item_id(
            &operation_id_for(&automatic.origin),
            &automatic.follow_ups[0].id,
        );

        std::thread::scope(|scope| {
            let probe = crate::test_lock_probe::ScopedLockProbe::install(
                "lineage",
                root_id,
                Some(("HUMAN", "ACQUIRED")),
            );
            let human_run = scope.spawn(|| {
                authorize_derived_work_item(tmp.path(), &store, "human-descendant")
            });
            assert!(probe.wait_for("HUMAN", "ACQUIRED"));

            let automatic_run = scope.spawn(|| process_landed_batch(tmp.path(), &automatic, None));
            assert!(probe.wait_for("AUTOMATIC", "BLOCKED"));
            probe.release();
            human_run.join().unwrap().unwrap();
            automatic_run.join().unwrap().unwrap();
        });

        let human = store.read_work_item("human-descendant").unwrap();
        let automatic = store.read_work_item(&automatic_id).unwrap();
        assert!(human.lineage.charged);
        assert!(!automatic.lineage.charged);
        assert!(automatic.authorization.is_proposed());
    }

    #[test]
    fn corrective_promotion_is_idempotent_across_replay() {
        let tmp = tempfile::TempDir::new().unwrap();
        seed_root_work_item(tmp.path());
        write_project_policy(tmp.path(), "follow-up:\n  mode: execute\n");
        let authority = fresh_expertise_authority(tmp.path());
        let batch = corrective_batch(tmp.path(), FollowUpSource::Learner, Some(authority));

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
    fn receipt_loss_replay_accepts_its_durable_dispatch_after_abandonment() {
        let tmp = tempfile::TempDir::new().unwrap();
        seed_root_work_item(tmp.path());
        write_project_policy(tmp.path(), "follow-up:\n  mode: execute\n");
        let authority = fresh_expertise_authority(tmp.path());
        let batch = corrective_batch(tmp.path(), FollowUpSource::Learner, Some(authority));
        let operation = record_post_land_operation(tmp.path(), &batch, None).unwrap();
        replay_post_land_operation(tmp.path(), &operation.operation_id).unwrap();

        let journal_path = operation_dir(tmp.path(), &operation.operation_id).join("journal.json");
        let mut journal = read_journal(&journal_path, &operation.operation_id).unwrap();
        journal.follow_ups[0].queued = false;
        write_journal(&journal_path, &journal).unwrap();
        let store = WorkModelStore::new(tmp.path());
        let mut derived = store.read_work_item(DERIVED_ID).unwrap();
        let expected_origin = derived
            .pending_enqueue
            .as_ref()
            .unwrap()
            .origin_operation_id
            .clone();
        derived.abandonment = Some(crate::work_model::WorkItemAbandonment {
            reason: Some("advanced after durable dispatch".to_string()),
        });
        store.write_work_item(&derived).unwrap();

        let replay = replay_post_land_operation(tmp.path(), &operation.operation_id).unwrap();

        assert_eq!(replay.work_items_queued, 0, "replay created no dispatch");
        let journal = read_journal(&journal_path, &operation.operation_id).unwrap();
        assert!(journal.follow_ups[0].queued);
        let ledger = crate::queue::read_ledger(tmp.path(), DERIVED_ID)
            .unwrap()
            .unwrap();
        assert_eq!(ledger.dispatches.len(), 1);
        assert_eq!(
            ledger.dispatches[0].origin_operation_id.as_deref(),
            Some(expected_origin.as_str())
        );
    }

    #[test]
    fn explicit_dispatch_cannot_be_stamped_as_the_automatic_queue_effect() {
        let tmp = tempfile::TempDir::new().unwrap();
        seed_root_work_item(tmp.path());
        write_project_policy(tmp.path(), "follow-up:\n  mode: execute\n");
        let authority = fresh_expertise_authority(tmp.path());
        let batch = corrective_batch(tmp.path(), FollowUpSource::Learner, Some(authority));
        let operation = record_post_land_operation(tmp.path(), &batch, None).unwrap();
        replay_post_land_operation(tmp.path(), &operation.operation_id).unwrap();
        let journal_path = operation_dir(tmp.path(), &operation.operation_id).join("journal.json");
        let mut journal = read_journal(&journal_path, &operation.operation_id).unwrap();
        journal.follow_ups[0].queued = false;
        write_journal(&journal_path, &journal).unwrap();

        fs::remove_file(
            tmp.path()
                .join(".fluent/work/queue")
                .join(format!("{DERIVED_ID}.json")),
        )
        .unwrap();
        crate::queue::add(tmp.path(), DERIVED_ID, Some(42)).unwrap();
        let unrelated = crate::queue::read_ledger(tmp.path(), DERIVED_ID)
            .unwrap()
            .unwrap();
        assert!(unrelated.latest().unwrap().origin_operation_id.is_none());

        let error = replay_post_land_operation(tmp.path(), &operation.operation_id)
            .unwrap_err()
            .to_string();

        assert!(error.contains("does not match automatic operation"));
        let journal = read_journal(&journal_path, &operation.operation_id).unwrap();
        assert!(!journal.follow_ups[0].queued);
        assert!(!post_land_operation_complete(tmp.path(), &operation.origin).unwrap());
    }

    #[test]
    fn corrective_retry_reuses_frozen_policy_after_config_change() {
        let tmp = tempfile::TempDir::new().unwrap();
        seed_root_work_item(tmp.path());
        write_project_policy(tmp.path(), "follow-up:\n  mode: execute\n");
        let authority = fresh_expertise_authority(tmp.path());
        let batch = corrective_batch(tmp.path(), FollowUpSource::Learner, Some(authority));

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
