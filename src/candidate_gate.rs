//! Run cheap, host-owned checks before Fluent spends a model-review cycle.
//! Recheck corrective candidates from their original base and admit a history
//! rewrite only when it repairs metadata without changing the candidate tree.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::Path;

#[cfg(test)]
use crate::verification_reference::*;
use crate::work_model::{CANDIDATE_METADATA_REPAIR_ROLE, WriterCompletionContract};

pub const CANDIDATE_GATE_SCHEMA_VERSION: u32 =
    crate::verification_assessment::VERIFICATION_GATE_SCHEMA_VERSION;
pub const CANDIDATE_GATE_FILE: &str = "candidate-gate.json";
const CANDIDATE_GATE_MAX_BYTES: u64 = 256 * 1024;
const MAX_PROJECT_CHECK_LOG_BYTES: u64 = 256 * 1024;
pub(crate) const COMMIT_PRESENTATION_ADVISORY_MARKER: &str =
    "# fluent-policy: commit-message-presentation-advisory-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CandidateGatePhase {
    Writer,
    FinalTester,
    Learner,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CandidateGateDisposition {
    Passed,
    Rejected,
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CandidateCheckStatus {
    Passed,
    Failed,
    Skipped,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateCheckEvidence {
    pub name: String,
    pub status: CandidateCheckStatus,
    pub diagnostics: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateGateEvidence {
    pub schema_version: u32,
    pub phase: CandidateGatePhase,
    pub disposition: CandidateGateDisposition,
    pub base_commit: String,
    pub candidate_commit: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tester_results_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata_rewrite: Option<CandidateMetadataRewriteEvidence>,
    pub checks: Vec<CandidateCheckEvidence>,
}

/// Immutable candidate-gate evidence that the current project policy has
/// retired without weakening any other candidate check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RetiredCommitStyleGate {
    pub candidate_tree: String,
    pub gate_digest: String,
    pub hook_log_path: String,
    pub hook_log_digest: String,
    pub findings: Vec<String>,
    pub policy_commit: String,
}

/// Host-verified provenance for a pre-review rewrite that changed only commit
/// metadata while preserving the rejected candidate's exact tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateMetadataRewriteEvidence {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prior_gate_path: Option<String>,
    #[serde(default)]
    pub authorization: CandidateMetadataRewriteAuthorization,
    pub original_base_commit: String,
    pub rejected_candidate_commit: String,
    pub replacement_candidate_commit: String,
    pub candidate_tree: String,
}

/// Exact host-owned authority for repairing a metadata rewrite that preserved
/// the candidate tree but accidentally removed the protected base from its
/// ancestry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateAncestryRepairAuthorization {
    pub protected_base_commit: String,
    pub rejected_candidate_commit: String,
    pub candidate_commit: String,
    pub candidate_tree: String,
    pub prior_gate_path: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CandidateMetadataRewriteAuthorization {
    #[default]
    RejectedMetadataGate,
    WriterCompletionRepair,
    ReviewFinding,
}

/// Host-validated authority for a tree-identical metadata rewrite authorized by
/// a completed review finding rather than a rejected candidate gate.
///
/// The Writer still supplies the replacement commit. The candidate gate uses
/// this context to verify that the replacement rewrites only commit metadata
/// after the protected base and keeps the source candidate's exact tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizedMetadataRewriteContext {
    pub source_candidate_commit: String,
    pub protected_base_commit: String,
    pub candidate_tree: String,
}

#[derive(Clone, Copy)]
pub struct CandidateGateRequest<'a> {
    pub project_root: &'a Path,
    pub workspace: &'a Path,
    pub work_item_id: &'a str,
    pub attempt_id: &'a str,
    pub task_id: Option<&'a str>,
    pub phase: CandidateGatePhase,
    pub base_commit: &'a str,
    pub candidate_commit: &'a str,
    pub completion_contract: Option<&'a WriterCompletionContract>,
    pub allow_tree_identical_metadata_rewrite: bool,
    pub metadata_rewrite_authorization: Option<&'a AuthorizedMetadataRewriteContext>,
    pub tester_results_path: Option<&'a Path>,
    /// Exact candidate-bound commands whose failed canonical Tester result was
    /// superseded by accepted host evidence and a passing targeted Tests review.
    pub accepted_tester_commands: &'a [String],
    pub artifact_path: &'a Path,
}

pub fn run_candidate_gate(request: CandidateGateRequest<'_>) -> Result<CandidateGateEvidence> {
    run_candidate_gate_with_prior(request, None)
}

pub(crate) fn run_candidate_gate_with_prior(
    request: CandidateGateRequest<'_>,
    prior_candidate_gate_path: Option<&Path>,
) -> Result<CandidateGateEvidence> {
    run_candidate_gate_with_prior_and_role(request, prior_candidate_gate_path, None)
}

/// Run the post-Writer candidate gate with the Writer role that produced the
/// candidate.  Metadata-recovery Writers must stay inside the audited,
/// tree-identical rewrite scope even when their replacement is a descendant of
/// the rejected candidate; ordinary Writers retain the append-only path.
pub(crate) fn run_candidate_gate_with_prior_and_role(
    request: CandidateGateRequest<'_>,
    prior_candidate_gate_path: Option<&Path>,
    writer_role: Option<&str>,
) -> Result<CandidateGateEvidence> {
    let evidence = assess_candidate_gate_with_prior_and_role(
        CandidateGateRequest { ..request },
        prior_candidate_gate_path,
        writer_role,
    )?;
    let parent = request
        .artifact_path
        .parent()
        .context("candidate gate artifact path has no parent")?;
    fs::create_dir_all(parent).with_context(|| {
        format!(
            "create candidate gate artifact directory {}",
            parent.display()
        )
    })?;
    let serialized = serde_json::to_vec_pretty(&evidence)?;
    crate::atomic_write::atomic_write(request.artifact_path, &serialized).with_context(|| {
        format!(
            "write candidate gate evidence {}",
            request.artifact_path.display()
        )
    })?;
    Ok(evidence)
}

/// Apply the Writer gate policy without publishing canonical gate evidence.
/// The caller chooses a private hook artifact directory for preflight.
pub(crate) fn assess_candidate_gate_with_prior_and_role(
    request: CandidateGateRequest<'_>,
    prior_candidate_gate_path: Option<&Path>,
    writer_role: Option<&str>,
) -> Result<CandidateGateEvidence> {
    let identity = check_candidate_identity(&request, prior_candidate_gate_path, writer_role)?;
    let mut checks = vec![
        identity.check,
        check_whitespace(&request, &identity.effective_base_commit)?,
        check_commit_metadata(&request, &identity.effective_base_commit)?,
    ];
    checks.extend(check_behavior_references(&request)?);
    checks.push(run_project_check(
        &request,
        &identity.effective_base_commit,
    )?);

    let disposition = if checks
        .iter()
        .any(|check| check.status == CandidateCheckStatus::Error)
    {
        CandidateGateDisposition::Blocked
    } else if checks
        .iter()
        .any(|check| check.status == CandidateCheckStatus::Failed)
    {
        CandidateGateDisposition::Rejected
    } else {
        CandidateGateDisposition::Passed
    };
    let evidence = CandidateGateEvidence {
        schema_version: CANDIDATE_GATE_SCHEMA_VERSION,
        phase: request.phase,
        disposition,
        base_commit: identity.effective_base_commit,
        candidate_commit: request.candidate_commit.to_string(),
        tester_results_path: request
            .tester_results_path
            .map(|path| evidence_path(request.project_root, path)),
        metadata_rewrite: identity.metadata_rewrite,
        checks,
    };
    Ok(evidence)
}

struct CandidateIdentityResolution {
    check: CandidateCheckEvidence,
    effective_base_commit: String,
    metadata_rewrite: Option<CandidateMetadataRewriteEvidence>,
}

fn check_candidate_identity(
    request: &CandidateGateRequest<'_>,
    prior_candidate_gate_path: Option<&Path>,
    writer_role: Option<&str>,
) -> Result<CandidateIdentityResolution> {
    let head = crate::git::run_stdout(
        request.workspace,
        &["rev-parse", "HEAD"],
        "resolve candidate gate HEAD",
    );
    let mut diagnostics = Vec::new();
    let mut effective_base_commit = request.base_commit.to_string();
    let mut metadata_rewrite = None;

    match head {
        Ok(head) if head == request.candidate_commit => {}
        Ok(head) => diagnostics.push(format!(
            "candidate workspace HEAD {head} does not match {}",
            request.candidate_commit
        )),
        Err(error) => diagnostics.push(format!("candidate HEAD cannot be resolved: {error:#}")),
    }

    if diagnostics.is_empty() {
        let worktree = crate::git::run_stdout(
            request.workspace,
            &["status", "--porcelain", "--untracked-files=all"],
            "verify candidate gate cleanliness",
        )?;
        if !worktree.is_empty() {
            diagnostics.push(format!(
                "candidate workspace is dirty before admission:\n{worktree}"
            ));
        }
    }

    if diagnostics.is_empty() {
        let metadata_recovery = writer_role == Some(CANDIDATE_METADATA_REPAIR_ROLE);
        match prior_candidate_gate_path {
            Some(path) => match read_prior_writer_gate(path) {
                Ok(prior) => {
                    if let Some(authorization) = request.metadata_rewrite_authorization {
                        // A review-derived rewrite starts from a passing gate.
                        // The older rejected-gate validator is intentionally
                        // strict for ordinary corrections, so use a separate
                        // passing-gate check for this explicitly audited path.
                        if let Err(reason) = validate_passing_writer_gate_for_metadata_rewrite(
                            &prior,
                            request.base_commit,
                        ) {
                            diagnostics.push(reason);
                        } else if !is_ancestor(
                            request.workspace,
                            &prior.base_commit,
                            &prior.candidate_commit,
                        )? {
                            diagnostics.push(format!(
                                "prior candidate {} does not descend from original base {}",
                                prior.candidate_commit, prior.base_commit
                            ));
                        } else {
                            match validate_authorized_metadata_rewrite(request, authorization)? {
                                Ok((base, evidence)) => {
                                    effective_base_commit = base;
                                    metadata_rewrite = Some(evidence);
                                }
                                Err(reason) => diagnostics.push(reason),
                            }
                        }
                    } else if let Err(reason) =
                        validate_prior_writer_gate(&prior, request.base_commit)
                    {
                        diagnostics.push(reason);
                    } else if !is_ancestor(
                        request.workspace,
                        &prior.base_commit,
                        &prior.candidate_commit,
                    )? {
                        diagnostics.push(format!(
                            "prior candidate {} does not descend from original base {}",
                            prior.candidate_commit, prior.base_commit
                        ));
                    } else {
                        effective_base_commit = prior.base_commit.clone();
                        let current_descends_from_rejected = is_ancestor(
                            request.workspace,
                            request.base_commit,
                            request.candidate_commit,
                        )?;
                        if metadata_recovery || !current_descends_from_rejected {
                            match validate_metadata_only_rewrite(request, path, &prior)? {
                                Ok(evidence) => metadata_rewrite = Some(evidence),
                                Err(reason) => diagnostics.push(reason),
                            }
                        }
                    }
                }
                Err(reason) => diagnostics.push(reason),
            },
            None => {
                if metadata_recovery {
                    diagnostics
                        .push("metadata-recovery Writer has no prior candidate gate".to_string());
                } else if is_ancestor(
                    request.workspace,
                    request.base_commit,
                    request.candidate_commit,
                )? {
                    // The normal append-only Writer path needs no recovery.
                } else if let Some(authorization) = request.metadata_rewrite_authorization {
                    match validate_authorized_metadata_rewrite(request, authorization)? {
                        Ok((base, evidence)) => {
                            effective_base_commit = base;
                            metadata_rewrite = Some(evidence);
                        }
                        Err(reason) => diagnostics.push(reason),
                    }
                } else if request.allow_tree_identical_metadata_rewrite {
                    match validate_completion_repair_metadata_rewrite(request)? {
                        Ok((base, evidence)) => {
                            effective_base_commit = base;
                            metadata_rewrite = Some(evidence);
                        }
                        Err(reason) => diagnostics.push(reason),
                    }
                } else {
                    diagnostics.push(format!(
                        "base commit {} is not an ancestor of candidate {}",
                        request.base_commit, request.candidate_commit
                    ));
                }
            }
        }
    }

    if diagnostics.is_empty()
        && !is_ancestor(
            request.workspace,
            &effective_base_commit,
            request.candidate_commit,
        )?
    {
        diagnostics.push(format!(
            "original base commit {effective_base_commit} is not an ancestor of candidate {}",
            request.candidate_commit
        ));
    }

    let status = if diagnostics.is_empty() {
        CandidateCheckStatus::Passed
    } else {
        CandidateCheckStatus::Error
    };
    Ok(CandidateIdentityResolution {
        check: check("candidate-identity", status, diagnostics),
        effective_base_commit,
        metadata_rewrite,
    })
}

fn validate_authorized_metadata_rewrite(
    request: &CandidateGateRequest<'_>,
    authorization: &AuthorizedMetadataRewriteContext,
) -> Result<std::result::Result<(String, CandidateMetadataRewriteEvidence), String>> {
    if request.base_commit != authorization.source_candidate_commit {
        return Ok(Err(format!(
            "metadata recovery expected source candidate {}, Writer reported base {}",
            authorization.source_candidate_commit, request.base_commit
        )));
    }
    if !is_ancestor(
        request.workspace,
        &authorization.protected_base_commit,
        request.base_commit,
    )? {
        return Ok(Err(format!(
            "protected base {} is not an ancestor of source candidate {}",
            authorization.protected_base_commit, request.base_commit
        )));
    }
    if !is_ancestor(
        request.workspace,
        &authorization.protected_base_commit,
        request.candidate_commit,
    )? {
        return Ok(Err(format!(
            "protected base {} is not an ancestor of replacement candidate {}",
            authorization.protected_base_commit, request.candidate_commit
        )));
    }
    let source_tree = crate::git::run_stdout(
        request.workspace,
        &[
            "rev-parse",
            &format!("{}^{{tree}}", authorization.source_candidate_commit),
        ],
        "resolve metadata recovery source tree",
    )?;
    let replacement_tree = crate::git::run_stdout(
        request.workspace,
        &[
            "rev-parse",
            &format!("{}^{{tree}}", request.candidate_commit),
        ],
        "resolve metadata recovery replacement tree",
    )?;
    if source_tree != authorization.candidate_tree {
        return Ok(Err(format!(
            "metadata recovery source tree {} does not match its authorized tree {}",
            source_tree, authorization.candidate_tree
        )));
    }
    if replacement_tree != authorization.candidate_tree {
        return Ok(Err(format!(
            "metadata recovery replacement tree {} does not match authorized tree {}",
            replacement_tree, authorization.candidate_tree
        )));
    }
    if let Some(reason) = equivalent_message_only_suffix_mismatch(
        request.workspace,
        &authorization.protected_base_commit,
        &authorization.source_candidate_commit,
        request.candidate_commit,
    )? {
        return Ok(Err(reason));
    }
    Ok(Ok((
        authorization.protected_base_commit.clone(),
        CandidateMetadataRewriteEvidence {
            prior_gate_path: None,
            authorization: CandidateMetadataRewriteAuthorization::ReviewFinding,
            original_base_commit: authorization.protected_base_commit.clone(),
            rejected_candidate_commit: authorization.source_candidate_commit.clone(),
            replacement_candidate_commit: request.candidate_commit.to_string(),
            candidate_tree: replacement_tree,
        },
    )))
}

fn validate_completion_repair_metadata_rewrite(
    request: &CandidateGateRequest<'_>,
) -> Result<std::result::Result<(String, CandidateMetadataRewriteEvidence), String>> {
    let base_tree = crate::git::run_stdout(
        request.workspace,
        &["rev-parse", &format!("{}^{{tree}}", request.base_commit)],
        "resolve completion-repair base tree",
    );
    let candidate_tree = crate::git::run_stdout(
        request.workspace,
        &[
            "rev-parse",
            &format!("{}^{{tree}}", request.candidate_commit),
        ],
        "resolve completion-repair candidate tree",
    );
    let (base_tree, candidate_tree) = match (base_tree, candidate_tree) {
        (Ok(base), Ok(candidate)) => (base, candidate),
        (Err(error), _) => {
            return Ok(Err(format!(
                "completion-repair base tree cannot be resolved: {error:#}"
            )));
        }
        (_, Err(error)) => {
            return Ok(Err(format!(
                "completion-repair candidate tree cannot be resolved: {error:#}"
            )));
        }
    };
    if base_tree != candidate_tree {
        return Ok(Err(format!(
            "completion-repair candidate tree {candidate_tree} does not match base tree {base_tree}"
        )));
    }
    let original_base = crate::git::run_stdout(
        request.workspace,
        &["merge-base", request.base_commit, request.candidate_commit],
        "resolve completion-repair original base",
    )?;
    if original_base.is_empty() {
        return Ok(Err(
            "completion-repair metadata rewrite has no common base".to_string()
        ));
    }
    Ok(Ok((
        original_base.clone(),
        CandidateMetadataRewriteEvidence {
            prior_gate_path: None,
            authorization: CandidateMetadataRewriteAuthorization::WriterCompletionRepair,
            original_base_commit: original_base,
            rejected_candidate_commit: request.base_commit.to_string(),
            replacement_candidate_commit: request.candidate_commit.to_string(),
            candidate_tree,
        },
    )))
}

fn is_ancestor(workspace: &Path, base: &str, candidate: &str) -> Result<bool> {
    Ok(
        crate::git::run_raw(workspace, &["merge-base", "--is-ancestor", base, candidate])?
            .status
            .success(),
    )
}

pub(crate) fn read_prior_writer_gate(
    path: &Path,
) -> std::result::Result<CandidateGateEvidence, String> {
    let metadata = fs::metadata(path)
        .map_err(|error| format!("prior candidate gate cannot be inspected: {error}"))?;
    if metadata.len() > CANDIDATE_GATE_MAX_BYTES {
        return Err(format!(
            "prior candidate gate is {} bytes; maximum is {CANDIDATE_GATE_MAX_BYTES} bytes",
            metadata.len()
        ));
    }
    let bytes =
        fs::read(path).map_err(|error| format!("prior candidate gate cannot be read: {error}"))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("prior candidate gate is malformed: {error}"))
}

pub(crate) fn validate_prior_writer_gate(
    prior: &CandidateGateEvidence,
    expected_candidate: &str,
) -> std::result::Result<(), String> {
    if prior.schema_version == 0 || prior.schema_version > CANDIDATE_GATE_SCHEMA_VERSION {
        return Err(format!(
            "prior candidate gate uses unsupported schema version {}",
            prior.schema_version
        ));
    }
    if !matches!(
        prior.phase,
        CandidateGatePhase::Writer | CandidateGatePhase::FinalTester
    ) || prior.disposition != CandidateGateDisposition::Rejected
    {
        return Err("prior candidate gate is not a rejected correction gate".to_string());
    }
    if prior.candidate_commit != expected_candidate {
        return Err(format!(
            "prior candidate gate names candidate {}, expected {expected_candidate}",
            prior.candidate_commit
        ));
    }
    let expected_checks = [
        "candidate-identity",
        "whitespace",
        "commit-metadata",
        "behavior-references",
        "tester-reference-evidence",
        "project-check",
    ];
    for name in expected_checks {
        if prior
            .checks
            .iter()
            .filter(|check| check.name == name)
            .count()
            != 1
        {
            return Err(format!(
                "prior candidate gate must contain exactly one {name} check"
            ));
        }
    }
    if prior.checks.len() != expected_checks.len() {
        return Err("prior candidate gate contains unknown or duplicate checks".to_string());
    }
    if prior
        .checks
        .iter()
        .any(|check| check.status == CandidateCheckStatus::Error)
    {
        return Err("prior candidate gate contains an untrustworthy check error".to_string());
    }
    if prior
        .checks
        .iter()
        .find(|check| check.name == "candidate-identity")
        .is_none_or(|check| check.status != CandidateCheckStatus::Passed)
    {
        return Err("prior candidate gate did not establish candidate identity".to_string());
    }
    Ok(())
}

/// Reclassify only the exact legacy project-check output that Factory's
/// checked-in policy now treats as presentation advice.
pub(crate) fn classify_retired_commit_style_gate(
    project_root: &Path,
    workspace: &Path,
    gate_path: &Path,
    expected_base: &str,
    expected_candidate: &str,
) -> Result<RetiredCommitStyleGate> {
    let gate_bytes = read_bounded_regular_file(
        gate_path,
        CANDIDATE_GATE_MAX_BYTES,
        "retired commit-style candidate gate",
    )?;
    let gate: CandidateGateEvidence =
        serde_json::from_slice(&gate_bytes).context("parse retired commit-style candidate gate")?;
    validate_prior_writer_gate(&gate, expected_candidate)
        .map_err(|reason| anyhow::anyhow!(reason))?;
    if gate.phase != CandidateGatePhase::Writer
        || gate.base_commit != expected_base
        || gate.tester_results_path.is_some()
        || gate.metadata_rewrite.is_some()
    {
        bail!("retired commit-style gate does not bind the exact source Writer candidate");
    }
    let failed = gate
        .checks
        .iter()
        .filter(|check| check.status == CandidateCheckStatus::Failed)
        .collect::<Vec<_>>();
    if failed.len() != 1 || failed[0].name != "project-check" {
        bail!("retired commit-style gate must fail only the project check");
    }
    for required in ["candidate-identity", "whitespace", "commit-metadata"] {
        if gate
            .checks
            .iter()
            .find(|check| check.name == required)
            .is_none_or(|check| check.status != CandidateCheckStatus::Passed)
        {
            bail!("retired commit-style gate did not pass {required}");
        }
    }
    let project_check = failed[0];
    let exit_codes = project_check
        .diagnostics
        .iter()
        .filter_map(|diagnostic| project_check_exit_code(diagnostic))
        .collect::<Vec<_>>();
    if project_check.diagnostics.len() != 1 || exit_codes.as_slice() != [1] {
        bail!("retired commit-style project check must record the canonical hook exit");
    }

    let expected_log_path = gate_path
        .parent()
        .context("retired commit-style gate path has no parent")?
        .join("hooks/check-pre-merge.log");
    let expected_log_relative = expected_log_path
        .strip_prefix(project_root)
        .map_err(|_| anyhow::anyhow!("retired commit-style gate is outside the project"))?
        .to_string_lossy()
        .to_string();
    if project_check.log_path.as_deref() != Some(expected_log_relative.as_str()) {
        bail!(
            "retired commit-style project check must bind the canonical hook log {expected_log_relative}"
        );
    }
    let log_bytes = read_bounded_regular_file(
        &expected_log_path,
        MAX_PROJECT_CHECK_LOG_BYTES,
        "retired commit-style hook log",
    )?;
    let log = String::from_utf8(log_bytes.clone())
        .map_err(|_| anyhow::anyhow!("retired commit-style hook log is not valid UTF-8"))?;
    let findings = log
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    if findings.is_empty() {
        bail!("retired commit-style hook log has no findings");
    }

    let commits = crate::git::run_stdout(
        workspace,
        &[
            "rev-list",
            "--reverse",
            &format!("{expected_base}..{expected_candidate}"),
        ],
        "enumerate retired commit-style candidate commits",
    )?
    .lines()
    .map(str::to_string)
    .collect::<Vec<_>>();
    if commits.is_empty() {
        bail!("retired commit-style candidate has no commits after its base");
    }
    // The candidate's own historical checker may have omitted a still-active
    // project blocker, so the stored presentation log is not sufficient proof
    // of attribution safety.
    for commit in &commits {
        let message = crate::git::run_stdout(
            workspace,
            &["show", "-s", "--format=%B", commit],
            "inspect retired commit-style attribution",
        )?;
        if message.lines().any(|line| {
            line.trim_start()
                .to_ascii_lowercase()
                .starts_with("co-authored-by:")
        }) {
            bail!(
                "retired commit-style candidate {commit} contains a prohibited Co-Authored-By trailer"
            );
        }
    }
    for finding in &findings {
        validate_retired_commit_style_finding(workspace, finding, &commits)?;
    }

    let policy_commit = crate::git::run_stdout(
        project_root,
        &["rev-parse", "HEAD"],
        "resolve commit-message presentation policy",
    )?;
    validate_retired_commit_style_policy(project_root, &policy_commit)?;

    let candidate_tree = crate::git::run_stdout(
        workspace,
        &["rev-parse", &format!("{expected_candidate}^{{tree}}")],
        "resolve retired commit-style candidate tree",
    )?;
    Ok(RetiredCommitStyleGate {
        candidate_tree,
        gate_digest: sha256_digest(&gate_bytes),
        hook_log_path: expected_log_relative,
        hook_log_digest: sha256_digest(&log_bytes),
        findings,
        policy_commit,
    })
}

fn validate_retired_commit_style_policy(project_root: &Path, policy_commit: &str) -> Result<()> {
    let resolved = crate::git::run_stdout(
        project_root,
        &[
            "rev-parse",
            "--verify",
            &format!("{policy_commit}^{{commit}}"),
        ],
        "resolve checked-in commit-message presentation policy",
    )?;
    if resolved != policy_commit {
        bail!("retired commit-style policy must name one exact commit");
    }
    let policy_script = crate::git::run_stdout(
        project_root,
        &[
            "show",
            &format!("{policy_commit}:scripts/check-commit-message-policy.sh"),
        ],
        "read checked-in commit-message presentation policy",
    )?;
    if !policy_script
        .lines()
        .any(|line| line == COMMIT_PRESENTATION_ADVISORY_MARKER)
    {
        bail!("the checked-in commit-message policy has not retired presentation findings");
    }
    let project_hook = crate::git::run_stdout(
        project_root,
        &[
            "show",
            &format!("{policy_commit}:.fluent/hooks/check-pre-merge"),
        ],
        "read checked-in project candidate hook",
    )?;
    if !project_hook.lines().any(|line| {
        let line = line.trim();
        !line.starts_with('#') && line == "scripts/check-commit-message-policy.sh"
    }) {
        bail!("the checked-in project hook does not invoke Factory's commit-message policy");
    }

    Ok(())
}

/// Keep the receipt's original policy commit as immutable audit evidence while
/// allowing ordinary project commits to advance the still-active policy.
pub(crate) fn validate_retired_commit_style_policy_replay(
    project_root: &Path,
    recorded_policy_commit: &str,
    current_policy_commit: &str,
) -> Result<()> {
    validate_retired_commit_style_policy(project_root, recorded_policy_commit)?;
    if !is_ancestor(project_root, recorded_policy_commit, current_policy_commit)? {
        bail!("retired commit-style policy commit is not an ancestor of the current policy");
    }
    Ok(())
}

fn read_bounded_regular_file(path: &Path, maximum: u64, subject: &str) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspect {subject} {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("{subject} is not a regular file: {}", path.display());
    }
    if metadata.len() > maximum {
        bail!(
            "{subject} is {} bytes; maximum is {maximum} bytes",
            metadata.len()
        );
    }
    fs::read(path).with_context(|| format!("read {subject} {}", path.display()))
}

fn validate_retired_commit_style_finding(
    workspace: &Path,
    diagnostic: &str,
    known_commits: &[String],
) -> Result<()> {
    let normalized = normalize_diagnostic_text(diagnostic);
    let rest = normalized
        .strip_prefix("commit ")
        .ok_or_else(|| anyhow::anyhow!("unknown retired commit-style hook diagnostic"))?;
    let (raw_commit, reason) = rest
        .split_once(' ')
        .ok_or_else(|| anyhow::anyhow!("malformed retired commit-style hook diagnostic"))?;
    let commit = resolve_commit_id(raw_commit.trim_end_matches(':'), known_commits)
        .ok_or_else(|| anyhow::anyhow!("retired commit-style finding has an unknown commit"))?;
    let message = crate::git::run_stdout(
        workspace,
        &["show", "-s", "--format=%B", &commit],
        "read retired commit-style message",
    )?;
    let lines = message.split('\n').collect::<Vec<_>>();
    if reason == "has no body separated from its subject by a blank line" {
        let has_body = lines.len() >= 3
            && lines[1].trim().is_empty()
            && lines[2..].iter().any(|line| !line.trim().is_empty());
        if has_body {
            bail!("retired missing-body finding does not match commit {commit}");
        }
        return Ok(());
    }
    let line_number = reason
        .strip_prefix("body line ")
        .and_then(|value| value.strip_suffix(" exceeds 72 characters"))
        .filter(|value| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|line| *line >= 2)
        .ok_or_else(|| anyhow::anyhow!("unknown retired commit-style hook diagnostic"))?;
    let line = lines
        .get(line_number - 1)
        .ok_or_else(|| anyhow::anyhow!("retired line-length finding names a missing line"))?;
    if line.chars().count() <= 72 {
        bail!("retired line-length finding does not match commit {commit}");
    }
    Ok(())
}

fn sha256_digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

/// Validate the passing source gate used by an explicitly authorized review
/// finding. This is deliberately separate from `validate_prior_writer_gate`,
/// which admits only rejected correction gates for the ordinary append-only
/// recovery path.
fn validate_passing_writer_gate_for_metadata_rewrite(
    prior: &CandidateGateEvidence,
    expected_candidate: &str,
) -> std::result::Result<(), String> {
    if prior.schema_version == 0 || prior.schema_version > CANDIDATE_GATE_SCHEMA_VERSION {
        return Err(format!(
            "source candidate gate uses unsupported schema version {}",
            prior.schema_version
        ));
    }
    if prior.phase != CandidateGatePhase::Writer
        || prior.disposition != CandidateGateDisposition::Passed
    {
        return Err("source candidate gate is not a passing Writer gate".to_string());
    }
    if prior.candidate_commit != expected_candidate {
        return Err(format!(
            "source candidate gate names candidate {}, expected {expected_candidate}",
            prior.candidate_commit
        ));
    }
    let expected_checks = [
        "candidate-identity",
        "whitespace",
        "commit-metadata",
        "behavior-references",
        "tester-reference-evidence",
        "project-check",
    ];
    if prior.checks.len() != expected_checks.len()
        || expected_checks.iter().any(|name| {
            prior
                .checks
                .iter()
                .filter(|check| check.name == *name)
                .count()
                != 1
        })
    {
        return Err("source candidate gate has an incomplete or duplicate check set".to_string());
    }
    if prior.checks.iter().any(|check| {
        matches!(
            check.status,
            CandidateCheckStatus::Failed | CandidateCheckStatus::Error
        )
    }) {
        return Err("source candidate gate contains a failed or untrustworthy check".to_string());
    }
    if prior
        .checks
        .iter()
        .find(|check| check.name == "candidate-identity")
        .is_none_or(|check| check.status != CandidateCheckStatus::Passed)
    {
        return Err("source candidate gate did not establish candidate identity".to_string());
    }
    Ok(())
}

/// Return the protected base when one rejected metadata gate authorizes an
/// exact tree-identical rewrite of `expected_candidate`.
pub(crate) fn authorized_tree_identical_metadata_rewrite_base(
    path: &Path,
    expected_candidate: &str,
) -> Option<String> {
    let prior = read_prior_writer_gate(path).ok()?;
    validate_prior_writer_gate(&prior, expected_candidate).ok()?;
    prior
        .checks
        .iter()
        .filter(|check| check.status == CandidateCheckStatus::Failed)
        .map(|check| check.name.as_str())
        .eq(["commit-metadata"])
        .then_some(prior.base_commit)
}

/// Recognize only the blocked gate shape produced when an otherwise valid,
/// tree-identical metadata repair replaced its protected base commit.
pub(crate) fn candidate_ancestry_repair_authorization(
    evidence: &CandidateGateEvidence,
) -> Option<CandidateAncestryRepairAuthorization> {
    if evidence.schema_version == 0
        || evidence.schema_version > CANDIDATE_GATE_SCHEMA_VERSION
        || evidence.phase != CandidateGatePhase::Writer
        || evidence.disposition != CandidateGateDisposition::Blocked
    {
        return None;
    }
    let rewrite = evidence.metadata_rewrite.as_ref()?;
    if rewrite.authorization != CandidateMetadataRewriteAuthorization::RejectedMetadataGate
        || evidence.base_commit != rewrite.original_base_commit
        || evidence.candidate_commit != rewrite.replacement_candidate_commit
    {
        return None;
    }
    let prior_gate_path = rewrite.prior_gate_path.clone()?;
    let expected_checks = [
        "candidate-identity",
        "whitespace",
        "commit-metadata",
        "behavior-references",
        "tester-reference-evidence",
        "project-check",
    ];
    if evidence.checks.len() != expected_checks.len()
        || expected_checks.iter().any(|name| {
            evidence
                .checks
                .iter()
                .filter(|check| check.name == *name)
                .count()
                != 1
        })
    {
        return None;
    }
    let expected_diagnostic = format!(
        "original base commit {} is not an ancestor of candidate {}",
        rewrite.original_base_commit, evidence.candidate_commit
    );
    let mut blocking_checks = evidence.checks.iter().filter(|check| {
        matches!(
            check.status,
            CandidateCheckStatus::Failed | CandidateCheckStatus::Error
        )
    });
    let identity = blocking_checks.next()?;
    if identity.name != "candidate-identity"
        || identity.status != CandidateCheckStatus::Error
        || identity.diagnostics.as_slice() != [expected_diagnostic]
        || blocking_checks.next().is_some()
    {
        return None;
    }
    Some(CandidateAncestryRepairAuthorization {
        protected_base_commit: rewrite.original_base_commit.clone(),
        rejected_candidate_commit: rewrite.rejected_candidate_commit.clone(),
        candidate_commit: evidence.candidate_commit.clone(),
        candidate_tree: rewrite.candidate_tree.clone(),
        prior_gate_path,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MetadataFinding {
    commit: String,
    reason: String,
}

/// Validate a project check that derives from the preceding gate's commit
/// metadata finding.  The gate owns this classifier so both the recovery
/// command and the candidate gate apply the same evidence contract.
pub(crate) fn validate_derived_project_check(
    project_root: &Path,
    gate_path: &Path,
    metadata_check: &CandidateCheckEvidence,
    project_check: &CandidateCheckEvidence,
) -> Result<String> {
    if metadata_check.status != CandidateCheckStatus::Failed
        || project_check.status != CandidateCheckStatus::Failed
    {
        bail!("derived project-check evidence must contain two failed checks");
    }
    let gate_findings = metadata_findings(project_root, &metadata_check.diagnostics, &[], false)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "project-check failure is not explained by structured commit-metadata diagnostics"
            )
        })?;
    if gate_findings.is_empty() {
        bail!("project-check failure is not explained by structured commit-metadata diagnostics");
    }
    let known_commits = gate_findings
        .iter()
        .map(|finding| finding.commit.clone())
        .fold(Vec::new(), |mut commits, commit| {
            if !commits.contains(&commit) {
                commits.push(commit);
            }
            commits
        });

    let expected_log_path = gate_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("candidate gate artifact path has no parent"))?
        .join("hooks/check-pre-merge.log");
    let expected_log_rel = expected_log_path
        .strip_prefix(project_root)
        .map_err(|_| anyhow::anyhow!("candidate gate artifact path is outside the project"))?
        .to_string_lossy()
        .to_string();
    if project_check.log_path.as_deref() != Some(expected_log_rel.as_str()) {
        bail!("derived project-check hook log must be the canonical {expected_log_rel} artifact");
    }

    let metadata = fs::symlink_metadata(&expected_log_path).with_context(|| {
        format!(
            "inspect derived project-check hook log {}",
            expected_log_path.display()
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!(
            "derived project-check hook log is not a regular file: {}",
            expected_log_path.display()
        );
    }
    if metadata.len() > MAX_PROJECT_CHECK_LOG_BYTES {
        bail!(
            "derived project-check hook log is {} bytes; maximum is {MAX_PROJECT_CHECK_LOG_BYTES} bytes",
            metadata.len()
        );
    }

    let exit_codes = project_check
        .diagnostics
        .iter()
        .filter_map(|diagnostic| project_check_exit_code(diagnostic))
        .collect::<Vec<_>>();
    if exit_codes.len() != 1 || exit_codes[0] == 0 {
        bail!(
            "derived project-check evidence must contain exactly one nonzero check-pre-merge exit"
        );
    }
    if project_check
        .diagnostics
        .iter()
        .any(|diagnostic| project_check_exit_code(diagnostic).is_none())
    {
        bail!("derived project-check diagnostics must be represented in the stored hook log");
    }

    let log = String::from_utf8(fs::read(&expected_log_path).with_context(|| {
        format!(
            "read derived project-check hook log {}",
            expected_log_path.display()
        )
    })?)
    .map_err(|_| anyhow::anyhow!("derived project-check hook log is not valid UTF-8"))?;
    let log_diagnostics = log
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    if log_diagnostics.is_empty() {
        bail!("derived project-check hook log has no structured hook diagnostics");
    }
    let log_findings = metadata_findings(project_root, &log_diagnostics, &known_commits, true)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "project-check hook log contains a diagnostic unrelated to commit-metadata findings"
            )
        })?;
    if log_findings.is_empty() {
        bail!("derived project-check hook log has no structured hook diagnostics");
    }
    if !log_diagnostics.iter().any(|diagnostic| {
        parse_metadata_finding(diagnostic, &known_commits, true).is_some_and(
            |(finding, raw_reason)| {
                is_body_line_length_reason(&raw_reason)
                    && gate_findings.iter().any(|gate| {
                        gate.commit == finding.commit && gate.reason == "literal-line-breaks"
                    })
            },
        )
    }) {
        bail!(
            "derived project-check hook log does not contain the required body-line-length finding"
        );
    }

    let diagnostic_findings = project_check
        .diagnostics
        .iter()
        .filter(|diagnostic| !is_project_check_exit_diagnostic(diagnostic))
        .cloned()
        .collect::<Vec<_>>();
    let diagnostic_findings = metadata_findings(
        project_root,
        &diagnostic_findings,
        &known_commits,
        true,
    )
    .ok_or_else(|| {
        anyhow::anyhow!(
            "project-check diagnostics contain a diagnostic unrelated to commit-metadata findings"
        )
    })?;
    if diagnostic_findings
        .iter()
        .any(|finding| !log_findings.contains(finding))
    {
        bail!("project-check diagnostics are not represented in the stored hook log");
    }

    let mut remaining = gate_findings;
    for finding in log_findings {
        let Some(index) = remaining.iter().position(|candidate| candidate == &finding) else {
            bail!(
                "project-check hook log contains a diagnostic unrelated to commit-metadata findings"
            );
        };
        remaining.remove(index);
    }
    Ok(expected_log_rel)
}

fn metadata_findings(
    project_root: &Path,
    diagnostics: &[String],
    known_commits: &[String],
    allow_project_policy_wording: bool,
) -> Option<Vec<MetadataFinding>> {
    diagnostics
        .iter()
        .map(|diagnostic| {
            let (finding, raw_reason) =
                parse_metadata_finding(diagnostic, known_commits, allow_project_policy_wording)?;
            if let Some(line_number) = project_policy_line_number(&raw_reason)
                && !commit_message_line_proves_literal_breaks(
                    project_root,
                    &finding.commit,
                    line_number,
                )?
            {
                return None;
            }
            Some(finding)
        })
        .collect()
}

fn parse_metadata_finding(
    diagnostic: &str,
    known_commits: &[String],
    allow_project_policy_wording: bool,
) -> Option<(MetadataFinding, String)> {
    let normalized = normalize_diagnostic_text(diagnostic);
    let rest = normalized.strip_prefix("commit ")?;
    let (raw_commit, reason) = rest.split_once(' ')?;
    let raw_commit = raw_commit.trim_end_matches(':');
    let commit = resolve_commit_id(raw_commit, known_commits)?;
    let raw_reason = reason.to_ascii_lowercase();
    let normalized_reason = normalize_metadata_reason(&raw_reason, allow_project_policy_wording)?;
    Some((
        MetadataFinding {
            commit,
            reason: normalized_reason,
        },
        raw_reason,
    ))
}

fn normalize_diagnostic_text(diagnostic: &str) -> String {
    diagnostic.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn resolve_commit_id(raw_commit: &str, known_commits: &[String]) -> Option<String> {
    if !(4..=64).contains(&raw_commit.len())
        || !raw_commit.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }
    let raw_commit = raw_commit.to_ascii_lowercase();
    if known_commits.is_empty() {
        return Some(raw_commit);
    }
    let mut matches = known_commits
        .iter()
        .filter(|commit| commit.as_str() == raw_commit || commit.starts_with(&raw_commit))
        .map(String::as_str)
        .collect::<Vec<_>>();
    matches.sort_unstable();
    matches.dedup();
    (matches.len() == 1).then(|| matches[0].to_string())
}

fn normalize_metadata_reason(reason: &str, allow_project_policy_wording: bool) -> Option<String> {
    if allow_project_policy_wording && is_body_line_length_reason(reason) {
        return Some("literal-line-breaks".to_string());
    }
    match reason {
        "is a merge commit" => Some("merge-commit".to_string()),
        "has an empty subject" => Some("empty-subject".to_string()),
        "contains literal \\n text instead of line breaks" => {
            Some("literal-line-breaks".to_string())
        }
        _ => reason.strip_prefix("has invalid ").and_then(|label| {
            let label = label.split(':').next()?.trim();
            matches!(
                label,
                "author name" | "author email" | "committer name" | "committer email"
            )
            .then(|| format!("invalid-{}", label.replace(' ', "-")))
        }),
    }
}

fn is_body_line_length_reason(reason: &str) -> bool {
    let mut fields = reason.split_whitespace();
    if fields.next() != Some("body") || fields.next() != Some("line") {
        return false;
    }
    let Some(line) = fields.next() else {
        return false;
    };
    let Ok(line_number) = line.parse::<u64>() else {
        return false;
    };
    if line.is_empty() || !line.bytes().all(|byte| byte.is_ascii_digit()) || line_number == 0 {
        return false;
    }
    matches!(
        (fields.next(), fields.next(), fields.next(), fields.next()),
        (Some("exceeds"), Some("72"), Some("characters"), None)
    )
}

fn project_policy_line_number(reason: &str) -> Option<usize> {
    let reason = reason.to_ascii_lowercase();
    let line_number = reason
        .strip_prefix("body line ")
        .and_then(|line| line.strip_suffix(" exceeds 72 characters"))?;
    (!line_number.is_empty() && line_number.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| line_number.parse::<usize>().ok())
        .flatten()
        .filter(|line| *line > 0)
}

fn commit_message_line_proves_literal_breaks(
    project_root: &Path,
    commit: &str,
    line_number: usize,
) -> Option<bool> {
    let output = crate::git::run_raw(project_root, &["show", "-s", "--format=%B", commit]).ok()?;
    if !output.status.success() {
        return None;
    }
    let message = String::from_utf8(output.stdout).ok()?;
    let line = message.split('\n').nth(line_number.checked_sub(1)?)?;
    Some(line.chars().count() > 72 && line.contains("\\n"))
}

fn is_project_check_exit_diagnostic(diagnostic: &str) -> bool {
    project_check_exit_code(diagnostic).is_some()
}

fn project_check_exit_code(diagnostic: &str) -> Option<i32> {
    let normalized = normalize_diagnostic_text(diagnostic);
    normalized
        .strip_prefix("check-pre-merge exited ")
        .and_then(|code| code.parse::<i32>().ok())
}

fn validate_metadata_only_rewrite(
    request: &CandidateGateRequest<'_>,
    prior_gate_path: &Path,
    prior: &CandidateGateEvidence,
) -> Result<std::result::Result<CandidateMetadataRewriteEvidence, String>> {
    let failed = prior
        .checks
        .iter()
        .filter(|check| check.status == CandidateCheckStatus::Failed)
        .map(|check| check.name.as_str())
        .collect::<Vec<_>>();
    if failed == ["commit-metadata", "project-check"] {
        let metadata_check = prior
            .checks
            .iter()
            .find(|check| check.name == "commit-metadata")
            .ok_or_else(|| anyhow::anyhow!("prior gate has no commit-metadata check"))?;
        let project_check = prior
            .checks
            .iter()
            .find(|check| check.name == "project-check")
            .ok_or_else(|| anyhow::anyhow!("prior gate has no project-check check"))?;
        if let Err(error) = validate_derived_project_check(
            request.project_root,
            prior_gate_path,
            metadata_check,
            project_check,
        ) {
            return Ok(Err(format!(
                "candidate history changed after prior gate failures: {error:#}"
            )));
        }
    } else if failed != ["commit-metadata"] {
        return Ok(Err(format!(
            "candidate history changed after prior gate failures: {}",
            if failed.is_empty() {
                "none".to_string()
            } else {
                failed.join(", ")
            }
        )));
    }
    let rejected_tree = crate::git::run_stdout(
        request.workspace,
        &["rev-parse", &format!("{}^{{tree}}", prior.candidate_commit)],
        "resolve rejected candidate tree",
    );
    let replacement_tree = crate::git::run_stdout(
        request.workspace,
        &[
            "rev-parse",
            &format!("{}^{{tree}}", request.candidate_commit),
        ],
        "resolve replacement candidate tree",
    );
    let (rejected_tree, replacement_tree) = match (rejected_tree, replacement_tree) {
        (Ok(rejected), Ok(replacement)) => (rejected, replacement),
        (Err(error), _) => {
            return Ok(Err(format!(
                "rejected candidate tree cannot be resolved: {error:#}"
            )));
        }
        (_, Err(error)) => {
            return Ok(Err(format!(
                "replacement candidate tree cannot be resolved: {error:#}"
            )));
        }
    };
    if rejected_tree != replacement_tree {
        return Ok(Err(format!(
            "rewritten candidate tree {replacement_tree} does not match rejected tree {rejected_tree}"
        )));
    }
    // A replacement that still descends from the protected base must preserve
    // the exact linear, per-commit tree/author suffix.  If it replaced that
    // base, retain the metadata-rewrite evidence so the dedicated ancestry
    // recovery can classify and repair the protected-base failure.
    if is_ancestor(
        request.workspace,
        &prior.base_commit,
        request.candidate_commit,
    )? && let Some(reason) = equivalent_message_only_suffix_mismatch(
        request.workspace,
        &prior.base_commit,
        &prior.candidate_commit,
        request.candidate_commit,
    )? {
        return Ok(Err(reason));
    }
    Ok(Ok(CandidateMetadataRewriteEvidence {
        prior_gate_path: Some(evidence_path(request.project_root, prior_gate_path)),
        authorization: CandidateMetadataRewriteAuthorization::RejectedMetadataGate,
        original_base_commit: prior.base_commit.clone(),
        rejected_candidate_commit: prior.candidate_commit.clone(),
        replacement_candidate_commit: request.candidate_commit.to_string(),
        candidate_tree: replacement_tree,
    }))
}

/// Return a diagnostic when a candidate rewrite changes anything beyond commit
/// messages.  The rejected and replacement tips must expose the same linear
/// suffix from the protected base, with identical trees and author identity at
/// each position.  Committer timestamps may change as a consequence of the
/// authorized rewrite; the candidate metadata gate still validates the new
/// committer identity and message.
fn equivalent_message_only_suffix_mismatch(
    workspace: &Path,
    protected_base: &str,
    rejected_candidate: &str,
    replacement_candidate: &str,
) -> Result<Option<String>> {
    let rejected = linear_commit_suffix(workspace, protected_base, rejected_candidate)?;
    let replacement = linear_commit_suffix(workspace, protected_base, replacement_candidate)?;
    if rejected.is_empty() || replacement.is_empty() {
        return Ok(Some(
            "metadata rewrite candidates must each contain a non-empty protected-base suffix"
                .to_string(),
        ));
    }
    if rejected.len() != replacement.len() {
        return Ok(Some(format!(
            "metadata rewrite changed the protected-base suffix length from {} to {}",
            rejected.len(),
            replacement.len()
        )));
    }
    for (old, new) in rejected.iter().zip(&replacement) {
        let old_identity = commit_author_identity(workspace, old)?;
        let new_identity = commit_author_identity(workspace, new)?;
        if old_identity != new_identity {
            return Ok(Some(format!(
                "metadata rewrite changed tree or author identity for commit {old}"
            )));
        }
    }
    Ok(None)
}

fn linear_commit_suffix(workspace: &Path, base: &str, tip: &str) -> Result<Vec<String>> {
    if !is_ancestor(workspace, base, tip)? {
        return Ok(Vec::new());
    }
    let raw = crate::git::run_stdout(
        workspace,
        &[
            "rev-list",
            "--reverse",
            "--first-parent",
            &format!("{base}..{tip}"),
        ],
        "enumerate metadata rewrite suffix",
    )?;
    let commits = raw.lines().map(str::to_string).collect::<Vec<_>>();
    let mut parent = base.to_string();
    for commit in &commits {
        let row = crate::git::run_stdout(
            workspace,
            &["rev-list", "--parents", "-n", "1", commit],
            "verify metadata rewrite linearity",
        )?;
        let fields = row.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 2 || fields[0] != commit || fields[1] != parent {
            return Ok(Vec::new());
        }
        parent = commit.clone();
    }
    Ok(commits)
}

fn commit_author_identity(workspace: &Path, commit: &str) -> Result<Vec<String>> {
    let raw = crate::git::run_stdout(
        workspace,
        &["show", "-s", "--format=%T%x00%an%x00%ae%x00%aI", commit],
        "read metadata rewrite author identity",
    )?;
    Ok(raw.split('\0').map(str::to_string).collect())
}

fn check_whitespace(
    request: &CandidateGateRequest<'_>,
    base_commit: &str,
) -> Result<CandidateCheckEvidence> {
    let range = format!("{}..{}", base_commit, request.candidate_commit);
    let output = crate::git::run_raw(request.workspace, &["diff", "--check", &range])?;
    if output.status.success() {
        Ok(check(
            "whitespace",
            CandidateCheckStatus::Passed,
            Vec::new(),
        ))
    } else {
        Ok(check(
            "whitespace",
            CandidateCheckStatus::Failed,
            output_diagnostics(&output),
        ))
    }
}

fn check_commit_metadata(
    request: &CandidateGateRequest<'_>,
    base_commit: &str,
) -> Result<CandidateCheckEvidence> {
    let range = format!("{base_commit}..{}", request.candidate_commit);
    let output = crate::git::run_raw(
        request.workspace,
        &[
            "log",
            "--format=%H%x1f%P%x1f%an%x1f%ae%x1f%cn%x1f%ce%x1f%B%x1e",
            &range,
        ],
    )?;
    if !output.status.success() {
        return Ok(check(
            "commit-metadata",
            CandidateCheckStatus::Error,
            output_diagnostics(&output),
        ));
    }
    let source = String::from_utf8_lossy(&output.stdout);
    let mut diagnostics = Vec::new();
    for record in source
        .split('\x1e')
        .filter(|record| !record.trim().is_empty())
    {
        let fields = record
            .trim_start_matches('\n')
            .splitn(7, '\x1f')
            .collect::<Vec<_>>();
        if fields.len() != 7 {
            diagnostics.push("Git returned malformed commit metadata".to_string());
            continue;
        }
        let commit = fields[0];
        let parents = fields[1].split_whitespace().count();
        if parents > 1 {
            diagnostics.push(format!("commit {commit} is a merge commit"));
        }
        for (label, value) in [
            ("author name", fields[2]),
            ("author email", fields[3]),
            ("committer name", fields[4]),
            ("committer email", fields[5]),
        ] {
            if value.trim().is_empty() || value.ends_with(".invalid") {
                diagnostics.push(format!("commit {commit} has invalid {label}: {value:?}"));
            }
        }
        let message = fields[6];
        if message
            .lines()
            .next()
            .is_none_or(|subject| subject.trim().is_empty())
        {
            diagnostics.push(format!("commit {commit} has an empty subject"));
        }
    }
    let status = if diagnostics.is_empty() {
        CandidateCheckStatus::Passed
    } else {
        CandidateCheckStatus::Failed
    };
    Ok(check("commit-metadata", status, diagnostics))
}

fn check_behavior_references(
    request: &CandidateGateRequest<'_>,
) -> Result<Vec<CandidateCheckEvidence>> {
    let contract = request.completion_contract;
    let results = request
        .tester_results_path
        .map(|path| {
            fs::read(path)
                .with_context(|| format!("read Tester evidence {}", path.display()))
                .and_then(|bytes| {
                    serde_json::from_slice::<crate::tester::TesterResults>(&bytes)
                        .context("cannot parse Tester evidence")
                })
        })
        .transpose();
    let results = match results {
        Ok(results) => results,
        Err(error) => {
            return Ok(vec![check(
                "tester-reference-evidence",
                CandidateCheckStatus::Error,
                vec![error.to_string()],
            )]);
        }
    };
    let saved = match crate::work_model::WorkModelStore::new(request.project_root)
        .read_work_item_snapshot(request.work_item_id)
    {
        Ok(item) => Some(item),
        Err(crate::work_model::WorkModelStorageError::ReadFile { source, path })
            if source.kind() == std::io::ErrorKind::NotFound
                && crate::work_model::WorkModelStore::new(request.project_root)
                    .work_item_path(request.work_item_id)
                    .is_ok_and(|root| root == path) =>
        {
            None
        }
        Err(error) => {
            return Ok(vec![check(
                "tester-reference-evidence",
                CandidateCheckStatus::Error,
                vec![format!(
                    "Current verification snapshot needs diagnosis: {error}"
                )],
            )]);
        }
    };
    let saved_attempt = saved.as_ref().and_then(|item| {
        item.attempts
            .iter()
            .find(|attempt| attempt.id == request.attempt_id)
    });
    let canonical = if let Some(attempt) = saved_attempt.filter(|attempt| {
        request.tester_results_path.is_some()
            && attempt.tasks.iter().any(|task| {
                task.kind == crate::work_model::TaskKind::Tester
                    && task.status == crate::work_model::TaskStatus::Complete
            })
    }) {
        match crate::verification_assessment::assess_canonical_checks(request.project_root, attempt)
        {
            Ok(view) => {
                if !request
                    .tester_results_path
                    .and_then(|path| fs::canonicalize(path).ok())
                    .zip(fs::canonicalize(&view.results_path).ok())
                    .is_some_and(|(supplied, recorded)| supplied == recorded)
                {
                    return Ok(vec![check("tester-reference-evidence", CandidateCheckStatus::Error,
                        vec!["Supplied Tester evidence is not the recorded current canonical artifact".into()])]);
                }
                if !view.blockers.is_empty() {
                    return Ok(vec![check(
                        "tester-reference-evidence",
                        CandidateCheckStatus::Error,
                        view.blockers
                            .iter()
                            .map(|reason| {
                                format!("Current canonical verification needs diagnosis: {reason}")
                            })
                            .collect(),
                    )]);
                }
                Some(view)
            }
            Err(error) => {
                return Ok(vec![check(
                    "tester-reference-evidence",
                    CandidateCheckStatus::Error,
                    vec![format!(
                        "Current verification bindings need diagnosis: {error:#}. Approved requirements: {}",
                        contract
                            .into_iter()
                            .flat_map(|contract| &contract.requirements)
                            .flat_map(|row| row
                                .verification_refs
                                .iter()
                                .map(move |reference| format!("{}: {reference}", row.id)))
                            .collect::<Vec<_>>()
                            .join("; ")
                    )],
                )]);
            }
        }
    } else {
        None
    };
    let Some(contract) = contract else {
        return Ok(vec![
            check(
                "behavior-references",
                CandidateCheckStatus::Skipped,
                vec!["Attempt has no Writer completion contract".to_string()],
            ),
            check(
                "tester-reference-evidence",
                CandidateCheckStatus::Skipped,
                vec!["Attempt has no Writer completion contract".to_string()],
            ),
        ]);
    };
    let mut adjusted = results.clone();
    if request.phase == CandidateGatePhase::Learner {
        if let Some(results) = &mut adjusted {
            if let Some(commit) = &results.candidate_commit {
                if commit != request.candidate_commit
                    && learner_only_delta(request.workspace, commit, request.candidate_commit)?
                {
                    results.candidate_commit = Some(request.candidate_commit.to_string());
                }
            }
        }
    }
    if adjusted.as_ref().is_some_and(|results| {
        results.candidate_commit.as_deref() != Some(request.candidate_commit)
    }) {
        return Ok(vec![check(
            "tester-reference-evidence",
            CandidateCheckStatus::Error,
            vec!["Tester evidence is not bound to this candidate commit".into()],
        )]);
    }
    if let (Some(results), Some(attempt), Some(canonical)) =
        (adjusted.as_mut(), saved_attempt, canonical.as_ref())
    {
        if let Some(writer_commit) = attempt
            .tasks
            .iter()
            .rev()
            .find(|task| {
                task.kind == crate::work_model::TaskKind::Write
                    && task.status == crate::work_model::TaskStatus::Complete
            })
            .and_then(|task| task.output.as_ref())
            .map(|output| output.commit.as_str())
        {
            results.tests.extend(
                crate::verification_assessment::accepted_native_observation_tests(
                    request.project_root,
                    attempt,
                    writer_commit,
                    canonical,
                    crate::check_applicability::evaluation_time(),
                ),
            );
        }
    }
    let bindings = request
        .tester_results_path
        .map(|path| crate::work_model::BoundRecheckArtifact {
            path: path.display().to_string(),
            digest: String::new(),
        })
        .into_iter()
        .collect::<Vec<_>>();
    let accepted = saved_attempt
        .map(|attempt| {
            crate::verification_assessment::accepted_legacy_commands(
                request.project_root,
                attempt,
                crate::check_applicability::evaluation_time(),
            )
        })
        .unwrap_or_default();
    let rows = crate::verification_assessment::assess_references(
        request.workspace,
        request.work_item_id,
        request.attempt_id,
        request.candidate_commit,
        contract,
        adjusted.as_ref(),
        canonical.as_ref(),
        &bindings,
        &accepted,
    )?;
    let mut source_diagnostics = Vec::new();
    for row in &rows {
        if row.cause == Some(crate::verification_assessment::VerificationCause::CandidateDefect) {
            source_diagnostics.extend(row.diagnostics.clone());
        }
    }
    let source_status = if source_diagnostics.is_empty() {
        CandidateCheckStatus::Passed
    } else {
        CandidateCheckStatus::Failed
    };
    let evidence_diagnostics = rows
        .iter()
        .filter(|row| {
            !row.satisfied
                && row.cause
                    != Some(crate::verification_assessment::VerificationCause::CandidateDefect)
        })
        .flat_map(|row| {
            let mut diagnostics = row.diagnostics.clone();
            if row.cause
                == Some(crate::verification_assessment::VerificationCause::UninterpretableEvidence)
            {
                diagnostics.push(format!(
                    "{}: {}",
                    row.id,
                    row.next_action
                        .as_deref()
                        .unwrap_or("Inspect retained evidence")
                ));
            }
            diagnostics
        })
        .collect::<Vec<_>>();
    let status = if rows.iter().any(|row| {
        row.cause
            == Some(crate::verification_assessment::VerificationCause::UninterpretableEvidence)
    }) {
        CandidateCheckStatus::Error
    } else if request.tester_results_path.is_none() {
        CandidateCheckStatus::Skipped
    } else if evidence_diagnostics.is_empty() {
        CandidateCheckStatus::Passed
    } else {
        CandidateCheckStatus::Failed
    };
    Ok(vec![
        check("behavior-references", source_status, source_diagnostics),
        check("tester-reference-evidence", status, evidence_diagnostics),
    ])
}

/// Resolve one exact command-shaped Test reference that a rejected final
/// candidate gate could not find in canonical Tester evidence. Selector-only
/// diagnostics require diagnosis before any source correction.
pub(crate) fn requested_host_evidence_command(evidence: &CandidateGateEvidence) -> Option<String> {
    crate::verification_assessment::requested_legacy_host_command(
        &serde_json::to_value(evidence).ok()?,
    )
}

fn learner_only_delta(workspace: &Path, from: &str, to: &str) -> Result<bool> {
    let output = crate::git::run_raw(workspace, &["diff", "--name-only", from, to])?;
    if !output.status.success() {
        return Ok(false);
    }
    let paths = String::from_utf8_lossy(&output.stdout);
    Ok(!paths.trim().is_empty()
        && paths
            .lines()
            .all(|path| path.starts_with(".fluent/expertise/")))
}

fn run_project_check(
    request: &CandidateGateRequest<'_>,
    base_commit: &str,
) -> Result<CandidateCheckEvidence> {
    if crate::hooks::find_hook(request.project_root, "check-pre-merge").is_none() {
        return Ok(check(
            "project-check",
            CandidateCheckStatus::Skipped,
            vec!["Project has no executable check-pre-merge hook".to_string()],
        ));
    }
    let before_head = crate::git::run_stdout(
        request.workspace,
        &["rev-parse", "HEAD"],
        "capture project-check HEAD",
    )?;
    let before_status = crate::git::run_stdout(
        request.workspace,
        &["status", "--porcelain", "--untracked-files=all"],
        "capture project-check status",
    )?;
    let log_dir = request
        .artifact_path
        .parent()
        .context("candidate gate artifact path has no parent")?
        .join("hooks");
    let context = crate::hooks::HookContext {
        work_item_id: Some(request.work_item_id.to_string()),
        attempt_id: Some(request.attempt_id.to_string()),
        task_id: request.task_id.map(str::to_string),
        candidate_commit: Some(request.candidate_commit.to_string()),
        base_commit: Some(base_commit.to_string()),
        artifact_dir: request.artifact_path.parent().map(Path::to_path_buf),
        log_dir,
        ..Default::default()
    };
    let outcome = match crate::hooks::run_hook(
        request.project_root,
        "check-pre-merge",
        request.workspace,
        &context,
    ) {
        Ok(Some(outcome)) => outcome,
        Ok(None) => {
            return Ok(check(
                "project-check",
                CandidateCheckStatus::Error,
                vec!["check-pre-merge disappeared before launch".to_string()],
            ));
        }
        Err(error) => {
            return Ok(check(
                "project-check",
                CandidateCheckStatus::Error,
                vec![format!("check-pre-merge could not run: {error:#}")],
            ));
        }
    };
    let after_head = crate::git::run_stdout(
        request.workspace,
        &["rev-parse", "HEAD"],
        "verify project-check HEAD",
    )?;
    let after_status = crate::git::run_stdout(
        request.workspace,
        &["status", "--porcelain", "--untracked-files=all"],
        "verify project-check status",
    )?;
    let log_path = Some(evidence_path(request.project_root, &outcome.log_path));
    if before_head != after_head || before_status != after_status {
        return Ok(CandidateCheckEvidence {
            name: "project-check".to_string(),
            status: CandidateCheckStatus::Error,
            diagnostics: vec![
                "check-pre-merge changed candidate HEAD or worktree state".to_string(),
            ],
            log_path,
        });
    }
    Ok(CandidateCheckEvidence {
        name: "project-check".to_string(),
        status: if outcome.passed {
            CandidateCheckStatus::Passed
        } else {
            CandidateCheckStatus::Failed
        },
        diagnostics: (!outcome.passed)
            .then(|| format!("check-pre-merge exited {}", outcome.exit_code))
            .into_iter()
            .collect(),
        log_path,
    })
}

fn check(
    name: &str,
    status: CandidateCheckStatus,
    diagnostics: Vec<String>,
) -> CandidateCheckEvidence {
    CandidateCheckEvidence {
        name: name.to_string(),
        status,
        diagnostics,
        log_path: None,
    }
}

fn output_diagnostics(output: &std::process::Output) -> Vec<String> {
    let mut diagnostics = Vec::new();
    for source in [&output.stdout, &output.stderr] {
        let text = String::from_utf8_lossy(source);
        diagnostics.extend(
            text.lines()
                .filter(|line| !line.trim().is_empty())
                .map(str::to_string),
        );
    }
    if diagnostics.is_empty() {
        diagnostics.push(format!(
            "command exited {}",
            output
                .status
                .code()
                .map_or_else(|| "after a signal".to_string(), |code| code.to_string())
        ));
    }
    diagnostics
}

fn evidence_path(project_root: &Path, path: &Path) -> String {
    path.strip_prefix(project_root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::work_model::{
        WRITER_COMPLETION_CONTRACT_VERSION, WriterCompletionRequirement,
        WriterCompletionRequirementKind,
    };
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    struct Fixture {
        project: tempfile::TempDir,
        root: tempfile::TempDir,
        artifacts: tempfile::TempDir,
        base: String,
    }

    impl Fixture {
        fn new() -> Self {
            let root = tempfile::TempDir::new().unwrap();
            crate::git::run(root.path(), &["init", "-q"], "initialize gate fixture").unwrap();
            crate::git::run(
                root.path(),
                &["config", "user.name", "Test Author"],
                "configure fixture author",
            )
            .unwrap();
            crate::git::run(
                root.path(),
                &["config", "user.email", "test@example.com"],
                "configure fixture email",
            )
            .unwrap();
            fs::write(root.path().join("README.md"), "fixture\n").unwrap();
            crate::git::run(root.path(), &["add", "README.md"], "stage fixture").unwrap();
            crate::git::run(
                root.path(),
                &["commit", "-q", "-m", "Create fixture"],
                "commit fixture",
            )
            .unwrap();
            let base =
                crate::git::run_stdout(root.path(), &["rev-parse", "HEAD"], "read base").unwrap();
            Self {
                project: tempfile::TempDir::new().unwrap(),
                root,
                artifacts: tempfile::TempDir::new().unwrap(),
                base,
            }
        }

        fn commit(&self, path: &str, contents: &str, message: &str) -> String {
            let path = self.root.path().join(path);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&path, contents).unwrap();
            crate::git::run(self.root.path(), &["add", "-A"], "stage candidate").unwrap();
            crate::git::run(
                self.root.path(),
                &["commit", "-q", "-m", message],
                "commit candidate",
            )
            .unwrap();
            crate::git::run_stdout(self.root.path(), &["rev-parse", "HEAD"], "read candidate")
                .unwrap()
        }

        fn legacy_literal_newline_gate(
            &self,
            candidate: &str,
            artifact: &Path,
        ) -> CandidateGateEvidence {
            let mut evidence =
                run_candidate_gate(self.request(candidate, None, None, artifact)).unwrap();
            // Older Fluent versions rejected this valid Git message content.
            let metadata = evidence
                .checks
                .iter_mut()
                .find(|check| check.name == "commit-metadata")
                .unwrap();
            metadata.status = CandidateCheckStatus::Failed;
            metadata.diagnostics = vec![format!(
                "commit {candidate} contains literal \\n text instead of line breaks"
            )];
            evidence.disposition = CandidateGateDisposition::Rejected;
            fs::write(artifact, serde_json::to_vec_pretty(&evidence).unwrap()).unwrap();
            evidence
        }

        fn request<'a>(
            &'a self,
            candidate: &'a str,
            contract: Option<&'a WriterCompletionContract>,
            tester_results_path: Option<&'a Path>,
            artifact_path: &'a Path,
        ) -> CandidateGateRequest<'a> {
            CandidateGateRequest {
                project_root: self.project.path(),
                workspace: self.root.path(),
                work_item_id: "work-1",
                attempt_id: "attempt-1",
                task_id: Some("attempt-1-write-1"),
                phase: CandidateGatePhase::Writer,
                base_commit: &self.base,
                candidate_commit: candidate,
                completion_contract: contract,
                allow_tree_identical_metadata_rewrite: false,
                metadata_rewrite_authorization: None,
                tester_results_path,
                accepted_tester_commands: &[],
                artifact_path,
            }
        }
    }

    #[test]
    fn assessed_writer_gate_does_not_publish_authoritative_evidence() {
        let fixture = Fixture::new();
        let candidate = fixture.commit("change.txt", "change\n", "Add change");
        let artifact = fixture.artifacts.path().join("candidate-gate.json");
        let request = fixture.request(&candidate, None, None, &artifact);
        let assessed = assess_candidate_gate_with_prior_and_role(request, None, None).unwrap();
        assert_eq!(assessed.disposition, CandidateGateDisposition::Passed);
        assert!(!artifact.exists());

        let published =
            run_candidate_gate(fixture.request(&candidate, None, None, &artifact)).unwrap();
        assert_eq!(assessed.checks, published.checks);
        assert!(artifact.exists());
    }

    fn behavior_contract(reference: &str) -> WriterCompletionContract {
        WriterCompletionContract {
            version: WRITER_COMPLETION_CONTRACT_VERSION,
            requirements: vec![WriterCompletionRequirement {
                responsibility: None,
                id: "behavior:area:b1".to_string(),
                kind: WriterCompletionRequirementKind::Behavior,
                source: "behaviors.md#area:b1".to_string(),
                requirement: "THE SYSTEM SHALL work.".to_string(),
                verification_refs: vec![reference.to_string()],
                command: None,
            }],
        }
    }

    fn check<'a>(evidence: &'a CandidateGateEvidence, name: &str) -> &'a CandidateCheckEvidence {
        evidence
            .checks
            .iter()
            .find(|check| check.name == name)
            .unwrap()
    }

    #[test]
    fn parses_supported_test_reference_forms() {
        let double_colon = parse_test_reference("tests/example.rs::module::works").unwrap();
        assert_eq!(double_colon.path, "tests/example.rs");
        assert_eq!(double_colon.selector.as_deref(), Some("module::works"));
        assert!(double_colon.command.is_none());

        let parenthesized = parse_test_reference("tests/example.rs (module::works)").unwrap();
        assert_eq!(parenthesized.path, "tests/example.rs");
        assert_eq!(parenthesized.selector.as_deref(), Some("module::works"));
        assert!(parenthesized.command.is_none());

        let path = parse_test_reference("tests/behaviors/example.sh").unwrap();
        assert_eq!(path.path, "tests/behaviors/example.sh");
        assert!(path.selector.is_none());
        assert!(path.command.is_none());
        assert!(parse_test_reference("tests/example.rs::").is_err());
    }

    #[test]
    fn derived_hook_classifier_normalizes_project_body_line_wording() {
        assert_eq!(
            normalize_metadata_reason("body line 2 exceeds 72 characters", true),
            Some("literal-line-breaks".to_string())
        );
        assert_eq!(
            normalize_metadata_reason("body line 0 exceeds 72 characters", true),
            None
        );
        assert_eq!(
            normalize_metadata_reason("body line 2 exceeds 73 characters", true),
            None
        );
        assert_eq!(
            normalize_metadata_reason(
                "has no body separated from its subject by a blank line",
                true,
            ),
            None
        );
    }

    #[test]
    fn parses_markdown_wrapped_source_path_test_reference() {
        let reference = parse_test_reference(
            "`src/candidate_metadata_recovery.rs` (metadata_recovery_accepts_derived_project_check)",
        )
        .unwrap();

        assert_eq!(reference.path, "src/candidate_metadata_recovery.rs");
        assert_eq!(
            reference.selector.as_deref(),
            Some("metadata_recovery_accepts_derived_project_check")
        );
        assert!(reference.command.is_none());
    }

    #[test]
    fn validates_markdown_wrapped_source_path_test_reference() {
        let fixture = Fixture::new();
        let candidate = fixture.commit(
            "src/candidate_metadata_recovery.rs",
            "#[test]\nfn metadata_recovery_accepts_derived_project_check() {}\n",
            "Add recovery test\n\n- Cover derived metadata checks",
        );
        let artifact = fixture.artifacts.path().join("candidate-gate.json");
        let contract = behavior_contract(
            "`src/candidate_metadata_recovery.rs` (metadata_recovery_accepts_derived_project_check)",
        );

        let evidence =
            run_candidate_gate(fixture.request(&candidate, Some(&contract), None, &artifact))
                .unwrap();

        assert_eq!(
            check(&evidence, "behavior-references").status,
            CandidateCheckStatus::Passed,
            "{:?}",
            check(&evidence, "behavior-references").diagnostics
        );
    }

    #[test]
    fn parses_python_command_reference_as_committed_script() {
        let reference = parse_test_reference(
            "PYTHONDONTWRITEBYTECODE=1 python3 -I tests/fluent_merge_readiness.py",
        )
        .unwrap();

        assert_eq!(reference.path, "tests/fluent_merge_readiness.py");
        assert!(reference.selector.is_none());
        assert_eq!(
            reference.command.as_deref(),
            Some("PYTHONDONTWRITEBYTECODE=1 python3 -I tests/fluent_merge_readiness.py")
        );
        assert!(parse_test_reference("python3 -m pytest tests/readiness.py").is_err());
        assert!(parse_test_reference("python3 tests/readiness.py | tee output").is_err());
    }

    #[test]
    fn python_command_reference_validates_committed_script_operand() {
        let fixture = Fixture::new();
        let candidate = fixture.commit(
            "tests/fluent_merge_readiness.py",
            "#!/usr/bin/env python3\n",
            "Add readiness journey\n\n- Cover the public workflow",
        );
        let artifact = fixture.artifacts.path().join("candidate-gate.json");
        let contract = behavior_contract("python3 tests/fluent_merge_readiness.py");

        let evidence =
            run_candidate_gate(fixture.request(&candidate, Some(&contract), None, &artifact))
                .unwrap();

        assert_eq!(
            check(&evidence, "behavior-references").status,
            CandidateCheckStatus::Passed
        );
    }

    #[test]
    fn markdown_wrapped_test_reference_validates_committed_path_and_selector() {
        let fixture = Fixture::new();
        let candidate = fixture.commit(
            "src/review.rs",
            "fn evidence_needed_requires_actionable_host_reason() {}\n",
            "Add review regression\n\n- Cover actionable host evidence",
        );
        let artifact = fixture.artifacts.path().join("candidate-gate.json");
        let contract =
            behavior_contract("`src/review.rs` (evidence_needed_requires_actionable_host_reason)");

        let evidence =
            run_candidate_gate(fixture.request(&candidate, Some(&contract), None, &artifact))
                .unwrap();

        assert_eq!(
            check(&evidence, "behavior-references").status,
            CandidateCheckStatus::Passed
        );
    }

    #[test]
    fn cargo_lib_reference_validates_committed_test_source() {
        let fixture = Fixture::new();
        let candidate = fixture.commit(
            "src/feature.rs",
            "#[test]\nfn verifies_assignment() {}\n",
            "Add assignment regression\n\n- Cover the Writer assignment",
        );
        let artifact = fixture.artifacts.path().join("candidate-gate.json");
        let contract = behavior_contract("`cargo test --lib verifies_assignment`");

        let evidence =
            run_candidate_gate(fixture.request(&candidate, Some(&contract), None, &artifact))
                .unwrap();

        assert_eq!(
            check(&evidence, "behavior-references").status,
            CandidateCheckStatus::Passed
        );
    }

    #[test]
    fn python_command_reference_requires_committed_script_operand() {
        let fixture = Fixture::new();
        let candidate = fixture.commit("README.md", "fixture changed\n", "Update fixture");
        let artifact = fixture.artifacts.path().join("candidate-gate.json");
        let contract = behavior_contract("python3 tests/missing.py");

        let evidence =
            run_candidate_gate(fixture.request(&candidate, Some(&contract), None, &artifact))
                .unwrap();

        assert!(
            check(&evidence, "behavior-references")
                .diagnostics
                .iter()
                .any(|line| line.contains("missing committed test path tests/missing.py"))
        );
    }

    #[test]
    fn accepts_literal_backslash_n_as_project_policy() {
        for message in [
            "Add value\\n\\nExplain value",
            "Add value\n\n- Explain literal \\n text",
        ] {
            let fixture = Fixture::new();
            let candidate = fixture.commit("src/lib.rs", "pub fn value() {}\n", message);
            let artifact = fixture.artifacts.path().join("candidate-gate.json");

            let evidence =
                run_candidate_gate(fixture.request(&candidate, None, None, &artifact)).unwrap();

            assert_eq!(evidence.disposition, CandidateGateDisposition::Passed);
            assert_eq!(
                check(&evidence, "commit-metadata").status,
                CandidateCheckStatus::Passed
            );
            assert!(check(&evidence, "commit-metadata").diagnostics.is_empty());
            assert!(evidence.metadata_rewrite.is_none());
        }
    }

    #[test]
    fn literal_backslash_n_does_not_mask_blocking_metadata() {
        for defect in ["identity", "empty subject"] {
            let fixture = Fixture::new();
            if defect == "identity" {
                crate::git::run(
                    fixture.root.path(),
                    &["config", "user.email", "writer@example.invalid"],
                    "set invalid identity",
                )
                .unwrap();
            }
            let mut candidate = fixture.commit(
                "src/lib.rs",
                "pub fn value() {}\n",
                "Add value\\n\\nExplain value",
            );
            if defect == "empty subject" {
                crate::git::run(
                    fixture.root.path(),
                    &[
                        "commit",
                        "--amend",
                        "-q",
                        "--cleanup=verbatim",
                        "--allow-empty-message",
                        "-m",
                        "\n\nExplain literal \\n text",
                    ],
                    "create empty subject",
                )
                .unwrap();
                candidate = crate::git::run_stdout(
                    fixture.root.path(),
                    &["rev-parse", "HEAD"],
                    "read candidate",
                )
                .unwrap();
            }
            let artifact = fixture.artifacts.path().join("candidate-gate.json");

            let evidence =
                run_candidate_gate(fixture.request(&candidate, None, None, &artifact)).unwrap();

            assert_eq!(
                evidence.disposition,
                CandidateGateDisposition::Rejected,
                "{defect}"
            );
            let metadata = check(&evidence, "commit-metadata");
            assert_eq!(metadata.status, CandidateCheckStatus::Failed);
            let expected = if defect == "identity" {
                vec!["invalid author email", "invalid committer email"]
            } else {
                vec!["has an empty subject"]
            };
            for reason in expected {
                assert!(
                    metadata
                        .diagnostics
                        .iter()
                        .any(|line| line.contains(reason)),
                    "{metadata:?}"
                );
            }
            assert!(
                metadata
                    .diagnostics
                    .iter()
                    .all(|line| !line.contains("literal"))
            );
        }
    }

    #[test]
    fn literal_backslash_n_does_not_mask_whitespace_or_project_failure() {
        for failed_check in ["whitespace", "project-check"] {
            let fixture = Fixture::new();
            let contents = if failed_check == "whitespace" {
                "pub fn value() {    \n}\n"
            } else {
                "pub fn value() {}\n"
            };
            let candidate = fixture.commit("src/lib.rs", contents, "Add value\\n\\nExplain value");
            if failed_check == "project-check" {
                let hooks = fixture.project.path().join(".fluent/hooks");
                fs::create_dir_all(&hooks).unwrap();
                let hook = hooks.join("check-pre-merge");
                fs::write(
                    &hook,
                    "#!/bin/sh\nprintf 'dependency cycle: app -> render -> app\\n'\nexit 9\n",
                )
                .unwrap();
                fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
            }
            let artifact = fixture.artifacts.path().join("candidate-gate.json");

            let evidence =
                run_candidate_gate(fixture.request(&candidate, None, None, &artifact)).unwrap();

            assert_eq!(evidence.disposition, CandidateGateDisposition::Rejected);
            assert_eq!(
                check(&evidence, "commit-metadata").status,
                CandidateCheckStatus::Passed
            );
            assert_eq!(
                check(&evidence, failed_check).status,
                CandidateCheckStatus::Failed
            );
            if failed_check == "project-check" {
                assert!(
                    fs::read_to_string(check(&evidence, failed_check).log_path.as_ref().unwrap())
                        .unwrap()
                        .contains("dependency cycle")
                );
            }
            let parsed: CandidateGateEvidence =
                serde_json::from_slice(&fs::read(artifact).unwrap()).unwrap();
            assert_eq!(parsed, evidence);
        }
    }

    #[test]
    fn rejects_merge_commits() {
        let merge_fixture = Fixture::new();
        let branch = crate::git::run_stdout(
            merge_fixture.root.path(),
            &["branch", "--show-current"],
            "read fixture branch",
        )
        .unwrap();
        merge_fixture.commit("src/main.rs", "pub fn main_value() {}\n", "Add main value");
        crate::git::run(
            merge_fixture.root.path(),
            &["checkout", "-q", "-b", "side", &merge_fixture.base],
            "create side branch",
        )
        .unwrap();
        merge_fixture.commit("src/side.rs", "pub fn side_value() {}\n", "Add side value");
        crate::git::run(
            merge_fixture.root.path(),
            &["checkout", "-q", &branch],
            "restore fixture branch",
        )
        .unwrap();
        crate::git::run(
            merge_fixture.root.path(),
            &[
                "merge",
                "--no-ff",
                "-q",
                "-m",
                "Merge side branch\\nExplain merge",
                "side",
            ],
            "create merge candidate",
        )
        .unwrap();
        let merge_candidate = crate::git::run_stdout(
            merge_fixture.root.path(),
            &["rev-parse", "HEAD"],
            "read merge candidate",
        )
        .unwrap();
        let merge_artifact = merge_fixture.artifacts.path().join("merge.json");
        let merge_evidence = run_candidate_gate(merge_fixture.request(
            &merge_candidate,
            None,
            None,
            &merge_artifact,
        ))
        .unwrap();
        assert!(
            check(&merge_evidence, "commit-metadata")
                .diagnostics
                .iter()
                .any(|line| line.contains("is a merge commit"))
        );
    }

    #[test]
    fn rejects_invalid_identities() {
        let identity_fixture = Fixture::new();
        crate::git::run(
            identity_fixture.root.path(),
            &["config", "user.email", "writer@example.invalid"],
            "configure invalid fixture identity",
        )
        .unwrap();
        let identity_candidate =
            identity_fixture.commit("src/lib.rs", "pub fn value() {}\n", "Add value");
        let identity_artifact = identity_fixture.artifacts.path().join("identity.json");
        let identity_evidence = run_candidate_gate(identity_fixture.request(
            &identity_candidate,
            None,
            None,
            &identity_artifact,
        ))
        .unwrap();
        assert!(
            check(&identity_evidence, "commit-metadata")
                .diagnostics
                .iter()
                .any(|line| line.contains("invalid author email"))
        );
    }

    #[test]
    fn rejects_an_invalid_append_only_ancestor() {
        let fixture = Fixture::new();
        crate::git::run(
            fixture.root.path(),
            &["config", "user.email", "writer@example.invalid"],
            "set invalid ancestor identity",
        )
        .unwrap();
        fixture.commit(
            "src/lib.rs",
            "pub fn value() {}\n",
            "Add value\\n\\nExplain value",
        );
        crate::git::run(
            fixture.root.path(),
            &["config", "user.email", "test@example.com"],
            "restore valid identity",
        )
        .unwrap();
        let corrected = fixture.commit(
            "src/lib.rs",
            "pub fn value() { println!(\"fixed\"); }\n",
            "Correct candidate metadata gate\n\n- Preserve the rejected commit as append-only history",
        );
        let artifact = fixture.artifacts.path().join("candidate-gate.json");

        let evidence =
            run_candidate_gate(fixture.request(&corrected, None, None, &artifact)).unwrap();

        assert_eq!(
            check(&evidence, "commit-metadata").status,
            CandidateCheckStatus::Failed
        );
    }

    #[test]
    fn accepts_tree_identical_metadata_rewrite_from_legacy_gate() {
        let fixture = Fixture::new();
        let rejected = fixture.commit(
            "src/lib.rs",
            "pub fn value() {}\n",
            "Add value\\n\\nExplain value",
        );
        let prior_artifact = fixture.artifacts.path().join("prior-gate.json");
        let prior = fixture.legacy_literal_newline_gate(&rejected, &prior_artifact);
        assert_eq!(prior.disposition, CandidateGateDisposition::Rejected);
        assert_eq!(
            prior
                .checks
                .iter()
                .filter(|check| check.status == CandidateCheckStatus::Failed)
                .map(|check| check.name.as_str())
                .collect::<Vec<_>>(),
            vec!["commit-metadata"]
        );
        let mut legacy: serde_json::Value =
            serde_json::from_slice(&fs::read(&prior_artifact).unwrap()).unwrap();
        legacy["schema_version"] = serde_json::json!(1);
        legacy.as_object_mut().unwrap().remove("metadata_rewrite");
        fs::write(&prior_artifact, serde_json::to_vec_pretty(&legacy).unwrap()).unwrap();

        crate::git::run(
            fixture.root.path(),
            &[
                "commit",
                "--amend",
                "-q",
                "-m",
                "Add value\n\n- Explain value",
            ],
            "rewrite candidate metadata",
        )
        .unwrap();
        let replacement = crate::git::run_stdout(
            fixture.root.path(),
            &["rev-parse", "HEAD"],
            "read replacement",
        )
        .unwrap();
        let artifact = fixture.artifacts.path().join("replacement-gate.json");
        let mut request = fixture.request(&replacement, None, None, &artifact);
        request.base_commit = &rejected;

        let evidence = run_candidate_gate_with_prior(request, Some(&prior_artifact)).unwrap();

        assert_eq!(evidence.disposition, CandidateGateDisposition::Passed);
        assert_eq!(evidence.base_commit, fixture.base);
        assert_eq!(
            check(&evidence, "commit-metadata").status,
            CandidateCheckStatus::Passed
        );
        let persisted: CandidateGateEvidence =
            serde_json::from_slice(&fs::read(&artifact).unwrap()).unwrap();
        assert_eq!(persisted, evidence);
        let rewrite = persisted.metadata_rewrite.unwrap();
        assert_eq!(rewrite.original_base_commit, fixture.base);
        assert_eq!(rewrite.rejected_candidate_commit, rejected);
        assert_eq!(rewrite.replacement_candidate_commit, replacement);
        assert_eq!(
            rewrite.prior_gate_path.as_deref(),
            Some(prior_artifact.to_string_lossy().as_ref())
        );
        assert_eq!(
            rewrite.authorization,
            CandidateMetadataRewriteAuthorization::RejectedMetadataGate
        );
    }

    #[test]
    fn blocks_metadata_rewrite_when_the_candidate_tree_changes() {
        let fixture = Fixture::new();
        let rejected = fixture.commit(
            "src/lib.rs",
            "pub fn value() {}\n",
            "Add value\\n\\nExplain value",
        );
        let prior_artifact = fixture.artifacts.path().join("prior-gate.json");
        fixture.legacy_literal_newline_gate(&rejected, &prior_artifact);
        fs::write(
            fixture.root.path().join("src/lib.rs"),
            "pub fn value() { 1 }\n",
        )
        .unwrap();
        crate::git::run(fixture.root.path(), &["add", "src/lib.rs"], "stage rewrite").unwrap();
        crate::git::run(
            fixture.root.path(),
            &[
                "commit",
                "--amend",
                "-q",
                "-m",
                "Add value\n\n- Correct the candidate metadata",
            ],
            "rewrite candidate and tree",
        )
        .unwrap();
        let replacement = crate::git::run_stdout(
            fixture.root.path(),
            &["rev-parse", "HEAD"],
            "read replacement",
        )
        .unwrap();
        let artifact = fixture.artifacts.path().join("replacement-gate.json");
        let mut request = fixture.request(&replacement, None, None, &artifact);
        request.base_commit = &rejected;

        let evidence = run_candidate_gate_with_prior(request, Some(&prior_artifact)).unwrap();

        assert_eq!(evidence.disposition, CandidateGateDisposition::Blocked);
        assert!(evidence.metadata_rewrite.is_none());
        assert!(
            check(&evidence, "candidate-identity")
                .diagnostics
                .iter()
                .any(|line| line.contains("does not match rejected tree"))
        );
    }

    #[test]
    fn blocks_metadata_rewrite_when_commit_author_changes() {
        let fixture = Fixture::new();
        let rejected = fixture.commit(
            "src/lib.rs",
            "pub fn value() {}\n",
            "Add value\\n\\nExplain value",
        );
        let prior_artifact = fixture.artifacts.path().join("prior-gate.json");
        fixture.legacy_literal_newline_gate(&rejected, &prior_artifact);

        crate::git::run(
            fixture.root.path(),
            &[
                "-c",
                "user.name=Different Author",
                "-c",
                "user.email=different@example.com",
                "commit",
                "--amend",
                "--reset-author",
                "-q",
                "-m",
                "Add value\n\n- Explain value",
            ],
            "rewrite candidate with a different author",
        )
        .unwrap();
        let replacement = crate::git::run_stdout(
            fixture.root.path(),
            &["rev-parse", "HEAD"],
            "read replacement",
        )
        .unwrap();
        let artifact = fixture.artifacts.path().join("replacement-gate.json");
        let mut request = fixture.request(&replacement, None, None, &artifact);
        request.base_commit = &rejected;

        let evidence = run_candidate_gate_with_prior(request, Some(&prior_artifact)).unwrap();

        assert_eq!(evidence.disposition, CandidateGateDisposition::Blocked);
        assert!(
            check(&evidence, "candidate-identity")
                .diagnostics
                .iter()
                .any(|line| line.contains("tree or author identity"))
        );
    }

    #[test]
    fn blocks_metadata_rewrite_when_history_shape_changes() {
        let fixture = Fixture::new();
        let rejected = fixture.commit(
            "src/lib.rs",
            "pub fn value() {}\n",
            "Add value\\n\\nExplain value",
        );
        let prior_artifact = fixture.artifacts.path().join("prior-gate.json");
        fixture.legacy_literal_newline_gate(&rejected, &prior_artifact);

        crate::git::run(
            fixture.root.path(),
            &["reset", "--hard", &fixture.base],
            "rebuild metadata rewrite suffix",
        )
        .unwrap();
        fs::create_dir_all(fixture.root.path().join("src")).unwrap();
        fs::write(
            fixture.root.path().join("src/lib.rs"),
            "pub fn value() {}\n",
        )
        .unwrap();
        crate::git::run(
            fixture.root.path(),
            &["add", "src/lib.rs"],
            "stage replacement",
        )
        .unwrap();
        crate::git::run(
            fixture.root.path(),
            &["commit", "-q", "-m", "Add value\n\n- Explain value"],
            "create first replacement commit",
        )
        .unwrap();
        crate::git::run(
            fixture.root.path(),
            &[
                "commit",
                "--allow-empty",
                "-q",
                "-m",
                "Record metadata repair",
            ],
            "create extra replacement commit",
        )
        .unwrap();
        let replacement = crate::git::run_stdout(
            fixture.root.path(),
            &["rev-parse", "HEAD"],
            "read replacement",
        )
        .unwrap();
        let artifact = fixture.artifacts.path().join("replacement-gate.json");
        let mut request = fixture.request(&replacement, None, None, &artifact);
        request.base_commit = &rejected;

        let evidence = run_candidate_gate_with_prior(request, Some(&prior_artifact)).unwrap();

        assert_eq!(evidence.disposition, CandidateGateDisposition::Blocked);
        assert!(
            check(&evidence, "candidate-identity")
                .diagnostics
                .iter()
                .any(|line| line.contains("suffix length"))
        );
    }

    #[test]
    fn blocks_history_rewrite_after_mixed_gate_failures() {
        let fixture = Fixture::new();
        let rejected = fixture.commit(
            "src/lib.rs",
            "pub fn value() {    \n}\n",
            "Add value\\n\\nExplain value",
        );
        let prior_artifact = fixture.artifacts.path().join("prior-gate.json");
        fixture.legacy_literal_newline_gate(&rejected, &prior_artifact);
        crate::git::run(
            fixture.root.path(),
            &[
                "commit",
                "--amend",
                "-q",
                "-m",
                "Add value\n\n- Keep the candidate tree unchanged",
            ],
            "rewrite candidate metadata",
        )
        .unwrap();
        let replacement = crate::git::run_stdout(
            fixture.root.path(),
            &["rev-parse", "HEAD"],
            "read replacement",
        )
        .unwrap();
        let artifact = fixture.artifacts.path().join("replacement-gate.json");
        let mut request = fixture.request(&replacement, None, None, &artifact);
        request.base_commit = &rejected;

        let evidence = run_candidate_gate_with_prior(request, Some(&prior_artifact)).unwrap();

        assert_eq!(evidence.disposition, CandidateGateDisposition::Blocked);
        let diagnostics = check(&evidence, "candidate-identity")
            .diagnostics
            .join("\n");
        assert!(diagnostics.contains("candidate history changed"));
        assert!(diagnostics.contains("whitespace"));
        assert!(diagnostics.contains("commit-metadata"));
    }

    #[test]
    fn blocks_non_ancestor_candidate_without_prior_gate() {
        let fixture = Fixture::new();
        let rejected = fixture.commit("src/lib.rs", "pub fn value() {}\n", "Add bad value");
        crate::git::run(
            fixture.root.path(),
            &["commit", "--amend", "-q", "-m", "Add value"],
            "rewrite candidate",
        )
        .unwrap();
        let replacement = crate::git::run_stdout(
            fixture.root.path(),
            &["rev-parse", "HEAD"],
            "read replacement",
        )
        .unwrap();
        let artifact = fixture.artifacts.path().join("replacement-gate.json");
        let mut request = fixture.request(&replacement, None, None, &artifact);
        request.base_commit = &rejected;

        let evidence = run_candidate_gate(request).unwrap();

        assert_eq!(evidence.disposition, CandidateGateDisposition::Blocked);
    }

    #[test]
    fn recognizes_only_tree_identical_protected_base_ancestry_failures() {
        let protected = "a".repeat(40);
        let rejected = "b".repeat(40);
        let candidate = "c".repeat(40);
        let diagnostic =
            format!("original base commit {protected} is not an ancestor of candidate {candidate}");
        let mut evidence = CandidateGateEvidence {
            schema_version: CANDIDATE_GATE_SCHEMA_VERSION,
            phase: CandidateGatePhase::Writer,
            disposition: CandidateGateDisposition::Blocked,
            base_commit: protected.clone(),
            candidate_commit: candidate.clone(),
            tester_results_path: None,
            metadata_rewrite: Some(CandidateMetadataRewriteEvidence {
                prior_gate_path: Some(".fluent/work/prior/candidate-gate.json".to_string()),
                authorization: CandidateMetadataRewriteAuthorization::RejectedMetadataGate,
                original_base_commit: protected.clone(),
                rejected_candidate_commit: rejected.clone(),
                replacement_candidate_commit: candidate.clone(),
                candidate_tree: "tree".to_string(),
            }),
            checks: vec![
                CandidateCheckEvidence {
                    name: "candidate-identity".to_string(),
                    status: CandidateCheckStatus::Error,
                    diagnostics: vec![diagnostic],
                    log_path: None,
                },
                CandidateCheckEvidence {
                    name: "whitespace".to_string(),
                    status: CandidateCheckStatus::Passed,
                    diagnostics: Vec::new(),
                    log_path: None,
                },
                CandidateCheckEvidence {
                    name: "commit-metadata".to_string(),
                    status: CandidateCheckStatus::Passed,
                    diagnostics: Vec::new(),
                    log_path: None,
                },
                CandidateCheckEvidence {
                    name: "behavior-references".to_string(),
                    status: CandidateCheckStatus::Passed,
                    diagnostics: Vec::new(),
                    log_path: None,
                },
                CandidateCheckEvidence {
                    name: "tester-reference-evidence".to_string(),
                    status: CandidateCheckStatus::Skipped,
                    diagnostics: Vec::new(),
                    log_path: None,
                },
                CandidateCheckEvidence {
                    name: "project-check".to_string(),
                    status: CandidateCheckStatus::Skipped,
                    diagnostics: Vec::new(),
                    log_path: None,
                },
            ],
        };

        let authorization = candidate_ancestry_repair_authorization(&evidence).unwrap();
        assert_eq!(authorization.protected_base_commit, protected);
        assert_eq!(authorization.rejected_candidate_commit, rejected);
        assert_eq!(authorization.candidate_commit, candidate);

        evidence.checks[1].status = CandidateCheckStatus::Failed;
        assert!(candidate_ancestry_repair_authorization(&evidence).is_none());
    }

    #[test]
    fn accepts_tree_identical_rewrite_for_completion_repair() {
        let fixture = Fixture::new();
        let rejected = fixture.commit(
            "src/lib.rs",
            "pub fn value() {}\n",
            "Add value\n\n- Keep the candidate tree unchanged",
        );
        crate::git::run(
            fixture.root.path(),
            &[
                "commit",
                "--amend",
                "-q",
                "-m",
                "Add value\n\n- Correct the candidate metadata",
            ],
            "rewrite candidate metadata",
        )
        .unwrap();
        let replacement = crate::git::run_stdout(
            fixture.root.path(),
            &["rev-parse", "HEAD"],
            "read replacement",
        )
        .unwrap();
        let artifact = fixture.artifacts.path().join("replacement-gate.json");
        let mut request = fixture.request(&replacement, None, None, &artifact);
        request.base_commit = &rejected;
        request.allow_tree_identical_metadata_rewrite = true;

        let evidence = run_candidate_gate(request).unwrap();

        assert_eq!(evidence.disposition, CandidateGateDisposition::Passed);
        assert_eq!(evidence.base_commit, fixture.base);
        let rewrite = evidence.metadata_rewrite.unwrap();
        assert_eq!(rewrite.prior_gate_path, None);
        assert_eq!(
            rewrite.authorization,
            CandidateMetadataRewriteAuthorization::WriterCompletionRepair
        );
        assert_eq!(rewrite.rejected_candidate_commit, rejected);
        assert_eq!(rewrite.replacement_candidate_commit, replacement);
    }

    #[test]
    fn completion_repair_rewrite_still_rejects_tree_changes() {
        let fixture = Fixture::new();
        let rejected = fixture.commit("src/lib.rs", "pub fn value() {}\n", "Add value");
        fs::write(
            fixture.root.path().join("src/lib.rs"),
            "pub fn value() { 1 }\n",
        )
        .unwrap();
        crate::git::run(fixture.root.path(), &["add", "src/lib.rs"], "stage rewrite").unwrap();
        crate::git::run(
            fixture.root.path(),
            &["commit", "--amend", "-q", "-m", "Add value"],
            "rewrite candidate tree",
        )
        .unwrap();
        let replacement = crate::git::run_stdout(
            fixture.root.path(),
            &["rev-parse", "HEAD"],
            "read replacement",
        )
        .unwrap();
        let artifact = fixture.artifacts.path().join("replacement-gate.json");
        let mut request = fixture.request(&replacement, None, None, &artifact);
        request.base_commit = &rejected;
        request.allow_tree_identical_metadata_rewrite = true;

        let evidence = run_candidate_gate(request).unwrap();

        assert_eq!(evidence.disposition, CandidateGateDisposition::Blocked);
        assert!(
            check(&evidence, "candidate-identity")
                .diagnostics
                .iter()
                .any(|line| line.contains("does not match base tree"))
        );
    }

    #[test]
    fn accepts_overlong_commit_body_lines_as_project_policy() {
        let fixture = Fixture::new();
        let candidate = fixture.commit(
            "src/lib.rs",
            "pub fn value() {}\n",
            &format!("Add value\n\n- {}", "x".repeat(73)),
        );
        let artifact = fixture.artifacts.path().join("candidate-gate.json");

        let evidence =
            run_candidate_gate(fixture.request(&candidate, None, None, &artifact)).unwrap();

        assert_eq!(evidence.disposition, CandidateGateDisposition::Passed);
        assert_eq!(
            check(&evidence, "commit-metadata").status,
            CandidateCheckStatus::Passed
        );
    }

    #[test]
    fn accepts_subject_only_and_coauthored_commits_as_project_policy() {
        let subject_only_fixture = Fixture::new();
        let subject_only = subject_only_fixture.commit(
            "src/subject_only.rs",
            "pub fn subject_only() {}\n",
            "Add subject-only candidate",
        );
        let coauthored_fixture = Fixture::new();
        let coauthored = coauthored_fixture.commit(
            "src/coauthored.rs",
            "pub fn coauthored() {}\n",
            "Add coauthored candidate\n\nCo-Authored-By: Bot <bot@example.com>",
        );

        let subject_only_artifact = subject_only_fixture
            .artifacts
            .path()
            .join("subject-only.json");
        let subject_only_evidence = run_candidate_gate(subject_only_fixture.request(
            &subject_only,
            None,
            None,
            &subject_only_artifact,
        ))
        .unwrap();
        assert_eq!(
            check(&subject_only_evidence, "commit-metadata").status,
            CandidateCheckStatus::Passed
        );

        let coauthored_artifact = coauthored_fixture.artifacts.path().join("coauthored.json");
        let coauthored_evidence = run_candidate_gate(coauthored_fixture.request(
            &coauthored,
            None,
            None,
            &coauthored_artifact,
        ))
        .unwrap();
        assert_eq!(
            check(&coauthored_evidence, "commit-metadata").status,
            CandidateCheckStatus::Passed
        );
    }

    #[test]
    fn malformed_prior_gate_blocks_history_rewrite() {
        let fixture = Fixture::new();
        let rejected = fixture.commit("src/lib.rs", "pub fn value() {}\n", "Add bad value");
        crate::git::run(
            fixture.root.path(),
            &["commit", "--amend", "-q", "-m", "Add value"],
            "rewrite candidate",
        )
        .unwrap();
        let replacement = crate::git::run_stdout(
            fixture.root.path(),
            &["rev-parse", "HEAD"],
            "read replacement",
        )
        .unwrap();
        let prior_artifact = fixture.artifacts.path().join("prior-gate.json");
        fs::write(&prior_artifact, "{not-json\n").unwrap();
        let artifact = fixture.artifacts.path().join("replacement-gate.json");
        let mut request = fixture.request(&replacement, None, None, &artifact);
        request.base_commit = &rejected;

        let evidence = run_candidate_gate_with_prior(request, Some(&prior_artifact)).unwrap();

        assert_eq!(evidence.disposition, CandidateGateDisposition::Blocked);
        assert!(
            check(&evidence, "candidate-identity")
                .diagnostics
                .iter()
                .any(|line| line.contains("malformed"))
        );
    }

    #[test]
    fn continuation_rechecks_the_complete_candidate_range() {
        let fixture = Fixture::new();
        let rejected = fixture.commit(
            "src/lib.rs",
            "pub fn value() {    \n}\n",
            "Add value\n\n- Explain the value",
        );
        let prior_artifact = fixture.artifacts.path().join("prior-gate.json");
        run_candidate_gate(fixture.request(&rejected, None, None, &prior_artifact)).unwrap();
        let replacement = fixture.commit(
            "src/lib.rs",
            "pub fn value() {}\n",
            "Clean value\n\n- Remove the whitespace error",
        );
        let artifact = fixture.artifacts.path().join("replacement-gate.json");
        let mut request = fixture.request(&replacement, None, None, &artifact);
        request.base_commit = &rejected;

        let evidence = run_candidate_gate_with_prior(request, Some(&prior_artifact)).unwrap();

        assert_eq!(evidence.disposition, CandidateGateDisposition::Passed);
        assert_eq!(evidence.base_commit, fixture.base);
        assert!(evidence.metadata_rewrite.is_none());
    }

    #[test]
    fn appended_commit_cannot_hide_prior_metadata_failure() {
        let fixture = Fixture::new();
        crate::git::run(
            fixture.root.path(),
            &["config", "user.email", "writer@example.invalid"],
            "set invalid ancestor identity",
        )
        .unwrap();
        let rejected = fixture.commit(
            "src/lib.rs",
            "pub fn value() {}\n",
            "Add value\\n\\nExplain value",
        );
        crate::git::run(
            fixture.root.path(),
            &["config", "user.email", "test@example.com"],
            "restore valid identity",
        )
        .unwrap();
        let prior_artifact = fixture.artifacts.path().join("prior-gate.json");
        run_candidate_gate(fixture.request(&rejected, None, None, &prior_artifact)).unwrap();
        let replacement = fixture.commit("src/extra.rs", "pub fn extra() {}\n", "Add extra");
        let artifact = fixture.artifacts.path().join("replacement-gate.json");
        let mut request = fixture.request(&replacement, None, None, &artifact);
        request.base_commit = &rejected;

        let evidence = run_candidate_gate_with_prior(request, Some(&prior_artifact)).unwrap();

        assert_eq!(evidence.disposition, CandidateGateDisposition::Rejected);
        assert_eq!(evidence.base_commit, fixture.base);
        assert_eq!(
            check(&evidence, "commit-metadata").status,
            CandidateCheckStatus::Failed
        );
    }

    #[test]
    fn mismatched_candidate_head_blocks_with_persisted_identity_evidence() {
        let fixture = Fixture::new();
        let candidate = fixture.commit("src/lib.rs", "pub fn value() {}\n", "Add value");
        let artifact = fixture.artifacts.path().join("candidate-gate.json");

        let evidence =
            run_candidate_gate(fixture.request(&fixture.base, None, None, &artifact)).unwrap();

        assert_ne!(candidate, fixture.base);
        assert_eq!(evidence.disposition, CandidateGateDisposition::Blocked);
        let identity = check(&evidence, "candidate-identity");
        assert_eq!(identity.status, CandidateCheckStatus::Error);
        assert!(
            identity
                .diagnostics
                .iter()
                .any(|line| line.contains("HEAD"))
        );
        assert!(artifact.is_file());
    }

    #[test]
    fn dirty_candidate_workspace_blocks_before_review() {
        let fixture = Fixture::new();
        let candidate = fixture.commit("src/lib.rs", "pub fn value() {}\n", "Add value");
        fs::write(fixture.root.path().join("untracked.txt"), "dirty\n").unwrap();
        let artifact = fixture.artifacts.path().join("candidate-gate.json");

        let evidence =
            run_candidate_gate(fixture.request(&candidate, None, None, &artifact)).unwrap();

        assert_eq!(evidence.disposition, CandidateGateDisposition::Blocked);
        let identity = check(&evidence, "candidate-identity");
        assert_eq!(identity.status, CandidateCheckStatus::Error);
        assert!(
            identity
                .diagnostics
                .iter()
                .any(|line| line.contains("dirty"))
        );
    }

    #[test]
    fn rejects_missing_test_paths_and_selectors_without_aliases() {
        let fixture = Fixture::new();
        let candidate = fixture.commit(
            "tests/dashboard.rs",
            "#[test]\nfn another_test() {}\n",
            "Add dashboard test\n\n- Cover dashboard selection",
        );
        let artifact = fixture.artifacts.path().join("candidate-gate.json");
        let contract = behavior_contract("tests/dashboard.rs (preserves_selection)");

        let evidence =
            run_candidate_gate(fixture.request(&candidate, Some(&contract), None, &artifact))
                .unwrap();

        assert_eq!(evidence.disposition, CandidateGateDisposition::Rejected);
        assert!(
            check(&evidence, "behavior-references")
                .diagnostics
                .iter()
                .any(|line| line.contains("preserves_selection"))
        );
    }

    #[test]
    fn validates_test_references_against_passing_structured_tester_evidence() {
        let fixture = Fixture::new();
        let candidate = fixture.commit(
            "tests/dashboard.rs",
            "#[test]\nfn preserves_selection() {}\n",
            "Add dashboard test\n\n- Cover dashboard selection",
        );
        let tester = fixture.artifacts.path().join("tester-results.json");
        fs::write(
            &tester,
            format!(
                "{{\"candidate_commit\":\"{candidate}\",\"commands\":[],\"tests\":[{{\"id\":\"dashboard::preserves_selection\",\"test_harness\":\"cargo-nextest\",\"status\":\"pass\",\"duration_ms\":1,\"failure_excerpt\":null}}],\"summary\":{{\"total\":1,\"pass\":1,\"fail\":0,\"skipped\":0}},\"error\":null}}"
            ),
        )
        .unwrap();
        let artifact = fixture.artifacts.path().join("candidate-gate.json");
        let contract = behavior_contract("tests/dashboard.rs (preserves_selection)");

        let evidence = run_candidate_gate(fixture.request(
            &candidate,
            Some(&contract),
            Some(&tester),
            &artifact,
        ))
        .unwrap();

        assert_eq!(evidence.disposition, CandidateGateDisposition::Passed);
        assert_eq!(
            check(&evidence, "tester-reference-evidence").status,
            CandidateCheckStatus::Passed
        );
    }

    const PYTHON_DECLARATION_WHITESPACE_SOURCES: &[&str] = &[
        "class\tTicketQueueTests:\n    def test_recovery_is_idempotent(self): pass\n",
        "class TicketQueueTests:\n    def\ttest_recovery_is_idempotent(self): pass\n",
        "class TicketQueueTests:\n    async\tdef test_recovery_is_idempotent(self): pass\n",
        "class TicketQueueTests:\n    async  def test_recovery_is_idempotent(self): pass\n",
        "class  TicketQueueTests:\n    def  test_recovery_is_idempotent(self): pass\n",
        "class \t TicketQueueTests:\n\tasync\t\tdef \t test_recovery_is_idempotent(self): pass\n",
    ];

    const PYTHON_DECLARATION_LOOKALIKES: &[&str] = &[
        "classTicketQueueTests:\n    def\ttest_recovery_is_idempotent(self): pass\n",
        "classes\tTicketQueueTests:\n    def\ttest_recovery_is_idempotent(self): pass\n",
        "class\tTicketQueueTests:\n    deftest_recovery_is_idempotent(self): pass\n",
        "class\tTicketQueueTests:\n    define\ttest_recovery_is_idempotent(self): pass\n",
        "class\tTicketQueueTests:\n    asyncdef\ttest_recovery_is_idempotent(self): pass\n",
        "class\tTicketQueueTests:\n    asynchronous\tdef\ttest_recovery_is_idempotent(self): pass\n",
        "class\tTicketQueueTests:\n    async\tdeftest_recovery_is_idempotent(self): pass\n",
        "class\tTicketQueueTests:\n    async  def\ttest_recovery_is_idempotent_suffix(self): pass\n",
        "class\tTicketQueueTestsSuffix:\n    async  def\ttest_recovery_is_idempotent(self): pass\n",
    ];

    #[test]
    fn accepts_class_qualified_python_unittest_reference() {
        for source in [
            "import unittest\n\nclass TicketQueueTests(unittest.TestCase):\n    def test_recovery_is_idempotent(self):\n        pass\n",
        ]
        .into_iter()
        .chain(PYTHON_DECLARATION_WHITESPACE_SOURCES.iter().copied())
        {
            let fixture = Fixture::new();
            let candidate = fixture.commit(
                "tests/test_ticket_queue.py",
                source,
                "Add queue test\n\n- Cover recovery",
            );
            let artifact = fixture.artifacts.path().join("candidate-gate.json");
            for reference in [
                "tests/test_ticket_queue.py::TicketQueueTests.test_recovery_is_idempotent",
                "tests/test_ticket_queue.py (TicketQueueTests.test_recovery_is_idempotent)",
                "tests/test_ticket_queue.py::test_recovery_is_idempotent",
            ] {
                let contract = behavior_contract(reference);
                let evidence =
                    run_candidate_gate(fixture.request(&candidate, Some(&contract), None, &artifact))
                        .unwrap();
                assert_eq!(
                    check(&evidence, "behavior-references").status,
                    CandidateCheckStatus::Passed,
                    "{source:?}, {reference}: {evidence:#?}"
                );
                assert_eq!(evidence.disposition, CandidateGateDisposition::Passed);
                assert_eq!(
                    check(&evidence, "tester-reference-evidence").status,
                    CandidateCheckStatus::Skipped
                );
            }
        }
    }

    #[test]
    fn resolves_python_declarations_without_executing_source() {
        let reference = parse_test_reference(
            "tests/test_ticket_queue.py::TicketQueueTests.test_recovery_is_idempotent",
        )
        .unwrap();
        for source in [
            "raise RuntimeError('do not execute candidate code')\nclass TicketQueueTests:\n    def test_recovery_is_idempotent(self): pass\n",
            "@decorate('class Fake:')\nclass TicketQueueTests(\n    unittest.TestCase,\n):\n    @decorate(\n        'def fake():',\n    )\n    def test_recovery_is_idempotent(\n        self,\n    ):\n        pass\n",
            "class TicketQueueTests:\r\n\tasync def test_recovery_is_idempotent(self): pass\r\n",
            "class TicketQueueTests:\n    def helper(self):\n        pass\n\n    # ignore a comment with unmatched delimiters: [ '\n    def test_recovery_is_idempotent(self):\n        text = '\\\"'\n",
        ]
        .into_iter()
        .chain(PYTHON_DECLARATION_WHITESPACE_SOURCES.iter().copied())
        {
            assert!(reference.source_contains_selector(source), "{source}");
        }
        for source in [
            "class TicketQueueTests:\n    def test_recovery_is_idempotent_suffix(self): pass\n",
            "class TicketQueueTests:\n    def test_recovery_is_idempotent(self): pass\nclass TicketQueueTests:\n    pass\n",
            "class TicketQueueTests:\n    if False:\n        def test_recovery_is_idempotent(self): pass\n",
            "class TicketQueueTests:\n    text = '''unterminated\n    def test_recovery_is_idempotent(self): pass\n",
            "class TicketQueueTests:\n    text = [)\n    def test_recovery_is_idempotent(self): pass\n",
            "text = (\n    '''class TicketQueueTests:\n    def test_recovery_is_idempotent(self): pass\n'''\n)\n",
        ]
        .into_iter()
        .chain(PYTHON_DECLARATION_LOOKALIKES.iter().copied())
        {
            assert!(!reference.source_contains_selector(source), "{source}");
        }
        for selector in [
            "TicketQueueTests..test_recovery_is_idempotent",
            "TicketQueueTests.test_recovery_is_idempotent()",
            "1Class.test_recovery_is_idempotent",
            "TicketQueueTests::test_recovery_is_idempotent",
        ] {
            let reference =
                parse_test_reference(&format!("tests/test_ticket_queue.py ({selector})")).unwrap();
            assert!(
                !reference.source_contains_selector(&format!("# {selector}\n")),
                "{selector}"
            );
        }
    }

    #[test]
    fn rejects_python_declaration_lookalikes() {
        for source in PYTHON_DECLARATION_LOOKALIKES {
            let fixture = Fixture::new();
            let candidate = fixture.commit(
                "tests/test_ticket_queue.py",
                source,
                "Add queue fixture\n\n- Cover declaration boundaries",
            );
            let artifact = fixture.artifacts.path().join("candidate-gate.json");
            let contract = behavior_contract(
                "tests/test_ticket_queue.py::TicketQueueTests.test_recovery_is_idempotent",
            );
            let evidence =
                run_candidate_gate(fixture.request(&candidate, Some(&contract), None, &artifact))
                    .unwrap();
            assert_eq!(
                check(&evidence, "behavior-references").status,
                CandidateCheckStatus::Failed,
                "{source:?}: {evidence:#?}"
            );
            assert_eq!(evidence.disposition, CandidateGateDisposition::Rejected);
        }
    }

    #[test]
    fn rejects_python_method_outside_referenced_class() {
        for source in [
            "class TicketQueueTests:\n    pass\n\nclass OtherTests:\n    def test_recovery_is_idempotent(self): pass\n",
            "class\tTicketQueueTests:\n    pass\n\nclass  OtherTests:\n    async  def\ttest_recovery_is_idempotent(self): pass\n",
            "class TicketQueueTests:\n    pass\n\ndef test_recovery_is_idempotent(): pass\n",
            "class\tTicketQueueTests:\n    pass\n\nasync\tdef\ttest_recovery_is_idempotent(): pass\n",
            "class TicketQueueTests:\n    def helper(self):\n        def test_recovery_is_idempotent(): pass\n",
            "class\tTicketQueueTests:\n    def\thelper(self):\n        async  def\ttest_recovery_is_idempotent(): pass\n",
            "class TicketQueueTests:\n    class Nested:\n        def test_recovery_is_idempotent(self): pass\n",
            "# TicketQueueTests.test_recovery_is_idempotent\nclass TicketQueueTests:\n    pass\n",
            "text = '''\nclass TicketQueueTests:\n    def test_recovery_is_idempotent(self): pass\n'''\n",
            "class TicketQueueTests:\n    text = '''\n    def test_recovery_is_idempotent(self): pass\n    '''\n",
        ] {
            let fixture = Fixture::new();
            let candidate = fixture.commit(
                "tests/test_ticket_queue.py",
                source,
                "Add queue fixture\n\n- Cover source ownership",
            );
            let artifact = fixture.artifacts.path().join("candidate-gate.json");
            let contract = behavior_contract(
                "tests/test_ticket_queue.py::TicketQueueTests.test_recovery_is_idempotent",
            );
            let evidence =
                run_candidate_gate(fixture.request(&candidate, Some(&contract), None, &artifact))
                    .unwrap();
            assert_eq!(
                check(&evidence, "behavior-references").status,
                CandidateCheckStatus::Failed,
                "{source}: {evidence:#?}"
            );
            assert_eq!(evidence.disposition, CandidateGateDisposition::Rejected);
        }
    }

    #[test]
    fn accepts_class_qualified_python_unittest_evidence() {
        let fixture = Fixture::new();
        let candidate = fixture.commit(
            "tests/test_ticket_queue.py",
            "class TicketQueueTests:\n    def test_recovery_is_idempotent(self): pass\n",
            "Add queue test\n\n- Cover recovery",
        );
        let tester = fixture.artifacts.path().join("tester-results.json");
        let artifact = fixture.artifacts.path().join("candidate-gate.json");
        let contract = behavior_contract(
            "tests/test_ticket_queue.py::TicketQueueTests.test_recovery_is_idempotent",
        );
        for identity in [
            "TicketQueueTests.test_recovery_is_idempotent",
            "tests.test_ticket_queue.TicketQueueTests.test_recovery_is_idempotent",
            "test_recovery_is_idempotent (tests.test_ticket_queue.TicketQueueTests)",
            "test_recovery_is_idempotent (tests.test_ticket_queue.TicketQueueTests.test_recovery_is_idempotent)",
            "tests/test_ticket_queue.py::TicketQueueTests.test_recovery_is_idempotent",
            "tests/test_ticket_queue.py (TicketQueueTests.test_recovery_is_idempotent)",
        ] {
            let results = serde_json::json!({
                "candidate_commit": candidate, "commands": [],
                "tests": [{"id": identity, "test_harness": "unittest", "status": "pass"}],
                "summary": {"total": 1, "pass": 1, "fail": 0, "skipped": 0}, "error": null
            });
            fs::write(&tester, serde_json::to_vec(&results).unwrap()).unwrap();
            let mut request =
                fixture.request(&candidate, Some(&contract), Some(&tester), &artifact);
            request.phase = CandidateGatePhase::FinalTester;
            let evidence = run_candidate_gate(request).unwrap();
            assert_eq!(
                evidence.disposition,
                CandidateGateDisposition::Passed,
                "{identity}: {evidence:#?}"
            );
            let preserved: serde_json::Value =
                serde_json::from_slice(&fs::read(&tester).unwrap()).unwrap();
            assert_eq!(preserved, results);
        }
    }

    #[test]
    fn resolves_short_python_evidence_without_bridging_classes() {
        let reference =
            parse_test_reference("tests/test_ticket_queue.py::test_recovery_is_idempotent")
                .unwrap();
        let qualified_reference = parse_test_reference(
            "tests/test_ticket_queue.py::TicketQueueTests.test_recovery_is_idempotent",
        )
        .unwrap();
        let cases: &[(&[(&str, &str)], bool, bool)] = &[
            (&[("test_recovery_is_idempotent", "pass")], true, true),
            (
                &[("TicketQueueTests.test_recovery_is_idempotent", "pass")],
                true,
                true,
            ),
            (
                &[
                    ("TicketQueueTests.test_recovery_is_idempotent", "pass"),
                    ("OtherTests.test_recovery_is_idempotent", "fail"),
                ],
                false,
                true,
            ),
            (
                &[
                    ("TicketQueueTests.test_recovery_is_idempotent", "pass"),
                    ("OtherTests.test_recovery_is_idempotent", "pass"),
                ],
                false,
                true,
            ),
            (
                &[
                    ("test_recovery_is_idempotent", "pass"),
                    ("OtherTests.test_recovery_is_idempotent", "pass"),
                ],
                true,
                false,
            ),
            (
                &[
                    ("test_recovery_is_idempotent", "pass"),
                    ("OtherTests.test_recovery_is_idempotent", "skipped"),
                ],
                false,
                false,
            ),
        ];
        for (identities, short_passes, qualified_passes) in cases {
            let mut tests = identities
                .iter()
                .map(|(id, status)| {
                    serde_json::from_value::<crate::tester::TestResult>(
                        serde_json::json!({"id": id, "test_harness": "unittest", "status": status}),
                    )
                    .unwrap()
                })
                .collect::<Vec<_>>();
            for _ in 0..2 {
                let parsed = tests
                    .iter()
                    .map(|test| (PythonTestIdentity::parse(&test.id).unwrap(), test))
                    .collect::<Vec<_>>();
                assert_eq!(
                    python_evidence_matches_reference(
                        &reference.python_identity().unwrap(),
                        &parsed
                    ),
                    *short_passes,
                    "{identities:?}"
                );
                assert_eq!(
                    python_evidence_matches_reference(
                        &qualified_reference.python_identity().unwrap(),
                        &parsed
                    ),
                    *qualified_passes,
                    "{identities:?}"
                );
                tests.reverse();
            }
        }
    }

    #[test]
    fn rejects_conflicting_class_qualified_python_evidence() {
        let fixture = Fixture::new();
        let candidate = fixture.commit(
            "tests/test_ticket_queue.py",
            "class TicketQueueTests:\n    def test_recovery_is_idempotent(self): pass\n",
            "Add queue test\n\n- Cover recovery",
        );
        let tester = fixture.artifacts.path().join("tester-results.json");
        let artifact = fixture.artifacts.path().join("candidate-gate.json");
        let contract = behavior_contract(
            "tests/test_ticket_queue.py::TicketQueueTests.test_recovery_is_idempotent",
        );
        let cases: &[&[(&str, &str)]] = &[
            &[],
            &[("OtherTests.test_recovery_is_idempotent", "pass")],
            &[("TicketQueueTests.test_missing", "pass")],
            &[("TicketQueueTests.test_recovery_is_idempotent", "fail")],
            &[("TicketQueueTests.test_recovery_is_idempotent", "skipped")],
            &[(
                "tests/other.py::TicketQueueTests.test_recovery_is_idempotent",
                "pass",
            )],
            &[
                ("test_recovery_is_idempotent", "pass"),
                ("OtherTests.test_recovery_is_idempotent", "fail"),
            ],
            &[
                ("TicketQueueTests.test_recovery_is_idempotent", "pass"),
                (
                    "test_recovery_is_idempotent (tests.test_ticket_queue.TicketQueueTests)",
                    "skipped",
                ),
            ],
            &[
                ("one.TicketQueueTests.test_recovery_is_idempotent", "pass"),
                ("two.TicketQueueTests.test_recovery_is_idempotent", "pass"),
            ],
            &[(
                "arbitrary text TicketQueueTests.test_recovery_is_idempotent",
                "pass",
            )],
            &[(
                "test_other (tests.test_ticket_queue.TicketQueueTests.test_recovery_is_idempotent)",
                "pass",
            )],
        ];
        for identities in cases {
            let tests = identities.iter().map(|(id, status)| serde_json::json!({"id": id, "test_harness": "unittest", "status": status})).collect::<Vec<_>>();
            let results = serde_json::json!({
                "candidate_commit": candidate, "commands": [], "tests": tests,
                "summary": {"total": tests.len(), "pass": identities.iter().filter(|(_, status)| *status == "pass").count(), "fail": identities.iter().filter(|(_, status)| *status == "fail").count(), "skipped": identities.iter().filter(|(_, status)| *status == "skipped").count()}, "error": null
            });
            fs::write(&tester, serde_json::to_vec(&results).unwrap()).unwrap();
            let mut request =
                fixture.request(&candidate, Some(&contract), Some(&tester), &artifact);
            request.phase = CandidateGatePhase::FinalTester;
            let evidence = run_candidate_gate(request).unwrap();
            assert_eq!(
                check(&evidence, "tester-reference-evidence").status,
                CandidateCheckStatus::Failed,
                "{identities:?}: {evidence:#?}"
            );
            assert_eq!(evidence.disposition, CandidateGateDisposition::Rejected);
        }
    }

    #[test]
    fn accepts_equivalent_native_rust_identities() {
        let fixture = Fixture::new();
        let candidate = fixture.commit(
            "src/lib.rs",
            "#[cfg(test)] mod tests { mod lifecycle { #[test] fn drain_is_idempotent() {} } }\n",
            "Add drain lifecycle\n\n- Cover idempotent drain",
        );
        let tester = fixture.artifacts.path().join("tester-results.json");
        let artifact = fixture.artifacts.path().join("candidate-gate.json");
        for (selector, identity, accepted) in [
            (
                "drain_is_idempotent",
                "my-crate::bin/tool drain_is_idempotent",
                true,
            ),
            (
                "drain_is_idempotent",
                "my-crate::bin/01-tool$tests::lifecycle::drain_is_idempotent",
                true,
            ),
            (
                "tests::lifecycle::drain_is_idempotent",
                "my-crate::bench/lifecycle tests::lifecycle::drain_is_idempotent",
                true,
            ),
            (
                "my-crate::example/tool$tests::lifecycle::drain_is_idempotent",
                "src/lib.rs (my-crate::example/tool tests::lifecycle::drain_is_idempotent)",
                true,
            ),
            (
                "my-crate::bin/tool$tests::lifecycle::drain_is_idempotent",
                "my-crate::example/tool tests::lifecycle::drain_is_idempotent",
                false,
            ),
            (
                "my-crate::bin/tool$tests::lifecycle::drain_is_idempotent",
                "my-crate::bin/other tests::lifecycle::drain_is_idempotent",
                false,
            ),
            (
                "drain_is_idempotent",
                "my-crate::bin/ drain_is_idempotent",
                false,
            ),
            (
                "drain_is_idempotent",
                "my-crate::/tool drain_is_idempotent",
                false,
            ),
            (
                "drain_is_idempotent",
                "::bin/tool drain_is_idempotent",
                false,
            ),
            (
                "drain_is_idempotent",
                "my-crate/bin/tool drain_is_idempotent",
                false,
            ),
            (
                "drain_is_idempotent",
                "my-crate::bin/tools/tool drain_is_idempotent",
                false,
            ),
            (
                "drain_is_idempotent",
                "my-crate::bin/tool::nested drain_is_idempotent",
                false,
            ),
            (
                "drain_is_idempotent",
                "my-crate::bin/tool tests/lifecycle/drain_is_idempotent",
                false,
            ),
            (
                "drain_is_idempotent",
                "src/lib.rs (tests::lifecycle::drain_is_idempotent)",
                true,
            ),
            ("drain_is_idempotent", "drain_is_idempotent", true),
            (
                "drain_is_idempotent",
                "tests::lifecycle::drain_is_idempotent",
                true,
            ),
            (
                "drain_is_idempotent",
                "src/lib.rs (drain_is_idempotent)",
                true,
            ),
            (
                "drain_is_idempotent",
                "src/lib.rs::tests::lifecycle::drain_is_idempotent",
                true,
            ),
            (
                "drain_is_idempotent",
                "fluent::lib$tests::lifecycle::drain_is_idempotent",
                true,
            ),
            (
                "drain_is_idempotent",
                "fluent::lib tests::lifecycle::drain_is_idempotent",
                true,
            ),
            (
                "tests::lifecycle::drain_is_idempotent",
                "src/lib.rs (tests::lifecycle::drain_is_idempotent)",
                true,
            ),
            (
                "tests::lifecycle::drain_is_idempotent",
                "fluent::lib$tests::lifecycle::drain_is_idempotent",
                true,
            ),
            (
                "fluent::lib$tests::lifecycle::drain_is_idempotent",
                "fluent::lib tests::lifecycle::drain_is_idempotent",
                true,
            ),
            (
                "drain_is_idempotent",
                "src/other.rs (tests::lifecycle::drain_is_idempotent)",
                false,
            ),
            (
                "drain_is_idempotent",
                "src/other.rs::drain_is_idempotent",
                false,
            ),
            (
                "tests::lifecycle::drain_is_idempotent",
                "other::lifecycle::drain_is_idempotent",
                false,
            ),
            (
                "lifecycle::drain_is_idempotent",
                "tests::lifecycle::drain_is_idempotent",
                false,
            ),
            (
                "fluent::lib$tests::lifecycle::drain_is_idempotent",
                "other::lib tests::lifecycle::drain_is_idempotent",
                false,
            ),
            (
                "drain_is_idempotent",
                "arbitrary display text drain_is_idempotent",
                false,
            ),
            (
                "drain_is_idempotent",
                "unrelated_drain_is_idempotent",
                false,
            ),
        ] {
            let results = serde_json::json!({
                "candidate_commit": candidate,
                "commands": [],
                "tests": [{"id": identity, "test_harness": "cargo-nextest", "status": "pass"}],
                "summary": {"total": 1, "pass": 1, "fail": 0, "skipped": 0},
                "error": null
            });
            fs::write(&tester, serde_json::to_vec(&results).unwrap()).unwrap();
            let contract = behavior_contract(&format!("src/lib.rs ({selector})"));
            let mut request =
                fixture.request(&candidate, Some(&contract), Some(&tester), &artifact);
            request.phase = CandidateGatePhase::FinalTester;
            let evidence = run_candidate_gate(request).unwrap();

            assert_eq!(
                check(&evidence, "behavior-references").status,
                CandidateCheckStatus::Passed,
                "source reference {selector}: {evidence:#?}"
            );
            assert_eq!(
                evidence.disposition,
                if accepted {
                    CandidateGateDisposition::Passed
                } else {
                    CandidateGateDisposition::Rejected
                },
                "reference {selector}, result {identity}: {evidence:#?}"
            );
            let preserved: serde_json::Value =
                serde_json::from_slice(&fs::read(&tester).unwrap()).unwrap();
            assert_eq!(preserved, results);
        }
    }

    #[test]
    fn rejects_invalid_or_ambiguous_test_evidence() {
        let fixture = Fixture::new();
        fixture.commit(
            "tests/lifecycle.sh",
            "#!/bin/sh\nexit 0\n",
            "Add lifecycle check\n\n- Exercise lifecycle commands",
        );
        let candidate = fixture.commit(
            "src/lib.rs",
            "#[cfg(test)] mod tests { mod a { #[test] fn works() {} } mod b { #[test] fn works() {} } }\n",
            "Add qualified tests\n\n- Cover module distinctions",
        );
        let tester = fixture.artifacts.path().join("tester-results.json");
        let artifact = fixture.artifacts.path().join("candidate-gate.json");
        let cases: &[(&str, &[(&str, &str)], bool)] = &[
            (
                "src/lib.rs (works)",
                &[
                    ("my-crate works", "pass"),
                    ("my-crate::bin/tool works", "skipped"),
                ],
                false,
            ),
            (
                "src/lib.rs (works)",
                &[
                    ("my-crate works", "pass"),
                    ("my-crate::bin/tool$works", "fail"),
                ],
                false,
            ),
            (
                "src/lib.rs (works)",
                &[
                    ("my-crate::bin/tool works", "pass"),
                    ("my-crate::bin/other$works", "skipped"),
                ],
                false,
            ),
            (
                "src/lib.rs (works)",
                &[
                    ("my-crate::bin/tool works", "pass"),
                    ("my-crate::example/tool$works", "fail"),
                ],
                false,
            ),
            (
                "src/lib.rs (my-crate::bin/tool$works)",
                &[
                    ("my-crate::bin/tool works", "pass"),
                    ("my-crate::example/tool$works", "skipped"),
                ],
                true,
            ),
            (
                "src/lib.rs (works)",
                &[("tests::a::works", "pass"), ("tests::b::works", "fail")],
                false,
            ),
            (
                "src/lib.rs (works)",
                &[("tests::a::works", "pass"), ("tests::b::works", "skipped")],
                false,
            ),
            (
                "src/lib.rs (works)",
                &[("tests::a::works", "pass"), ("tests::b::works", "pass")],
                false,
            ),
            (
                "src/lib.rs (works)",
                &[
                    ("one$tests::a::works", "pass"),
                    ("two tests::a::works", "fail"),
                ],
                false,
            ),
            (
                "src/lib.rs (works)",
                &[
                    ("one$tests::a::works", "pass"),
                    ("two tests::a::works", "skipped"),
                ],
                false,
            ),
            (
                "src/lib.rs (works)",
                &[
                    ("src/lib.rs (tests::a::works)", "pass"),
                    ("src/lib.rs (tests::b::works)", "fail"),
                ],
                false,
            ),
            (
                "src/lib.rs (works)",
                &[("tests::a::works", "pass"), ("tests::a::works", "fail")],
                false,
            ),
            (
                "src/lib.rs (works)",
                &[
                    ("tests::a::works", "pass"),
                    ("src/lib.rs (tests::a::works)", "skipped"),
                ],
                false,
            ),
            (
                "src/lib.rs (tests::a::works)",
                &[("works", "pass"), ("tests::b::works", "fail")],
                false,
            ),
            ("src/lib.rs (works)", &[], false),
            ("src/lib.rs (works)", &[("tests::a::works", "fail")], false),
            (
                "src/lib.rs (works)",
                &[("tests::a::works", "skipped")],
                false,
            ),
            (
                "src/lib.rs (works)",
                &[("tests::a::works", "not_run")],
                false,
            ),
            (
                "src/lib.rs (works)",
                &[("tests::a::unrelated", "pass")],
                false,
            ),
            (
                "src/lib.rs (works)",
                &[("src/other.rs (works)", "pass")],
                false,
            ),
            (
                "src/lib.rs (works)",
                &[("src/lib.rs (works", "pass")],
                false,
            ),
            (
                "src/lib.rs (works)",
                &[("arbitrary prose works", "pass")],
                false,
            ),
            ("src/lib.rs (works)", &[("tests::a::works", "pass")], true),
            (
                "src/lib.rs (works)",
                &[
                    ("tests::a::works", "pass"),
                    ("src/lib.rs (tests::a::works)", "pass"),
                ],
                true,
            ),
            (
                "src/lib.rs (tests::a::works)",
                &[("tests::a::works", "pass"), ("tests::b::works", "fail")],
                true,
            ),
            (
                "src/lib.rs (one$tests::a::works)",
                &[
                    ("one tests::a::works", "pass"),
                    ("two$tests::a::works", "skipped"),
                ],
                true,
            ),
            (
                "src/lib.rs (works)",
                &[
                    ("src/lib.rs (tests::a::works)", "pass"),
                    ("src/other.rs (tests::a::works)", "fail"),
                ],
                true,
            ),
            (
                "tests/lifecycle.sh (covers lifecycle)",
                &[
                    ("tests/lifecycle.sh", "pass"),
                    ("tests/lifecycle.sh", "fail"),
                ],
                false,
            ),
        ];
        for (reference, rows, accepted) in cases {
            // Evidence order must never decide which same-named test wins.
            for reverse in [false, true] {
                let mut tests = rows
                    .iter()
                    .map(|(id, status)| {
                        serde_json::json!({
                            "id": id, "test_harness": "project checks", "status": status
                        })
                    })
                    .collect::<Vec<_>>();
                if reverse {
                    tests.reverse();
                }
                let results = serde_json::json!({
                    "candidate_commit": candidate,
                    "commands": [],
                    "tests": tests,
                    "summary": {
                        "total": rows.len(),
                        "pass": rows.iter().filter(|(_, status)| *status == "pass").count(),
                        "fail": rows.iter().filter(|(_, status)| *status == "fail").count(),
                        "skipped": rows.iter().filter(|(_, status)| *status == "skipped").count()
                    },
                    "error": null
                });
                fs::write(&tester, serde_json::to_vec(&results).unwrap()).unwrap();
                let contract = behavior_contract(reference);
                let mut request =
                    fixture.request(&candidate, Some(&contract), Some(&tester), &artifact);
                request.phase = CandidateGatePhase::FinalTester;
                let evidence = run_candidate_gate(request).unwrap();
                assert_eq!(
                    check(&evidence, "behavior-references").status,
                    CandidateCheckStatus::Passed
                );
                let tester_check = check(&evidence, "tester-reference-evidence");
                assert_eq!(
                    tester_check.status,
                    if *accepted {
                        CandidateCheckStatus::Passed
                    } else {
                        CandidateCheckStatus::Failed
                    },
                    "{reference}, {rows:?}, reversed={reverse}: {evidence:#?}"
                );
                assert_eq!(
                    evidence.disposition,
                    if *accepted {
                        CandidateGateDisposition::Passed
                    } else {
                        CandidateGateDisposition::Rejected
                    }
                );
                if !accepted {
                    assert!(
                        tester_check
                            .diagnostics
                            .iter()
                            .any(|line| line.contains(reference)),
                        "{tester_check:#?}"
                    );
                    for (id, _) in *rows {
                        assert!(
                            tester_check
                                .diagnostics
                                .iter()
                                .any(|line| line.contains(id)),
                            "missing observed {id}: {tester_check:#?}"
                        );
                    }
                }
                let preserved: serde_json::Value =
                    serde_json::from_slice(&fs::read(&tester).unwrap()).unwrap();
                assert_eq!(preserved, results);
            }
        }

        for (reference, id) in [
            ("src/lib.rs (works)", "tests::a::works"),
            (
                "tests/lifecycle.sh (covers lifecycle)",
                "tests/lifecycle.sh",
            ),
        ] {
            for commit in [Some(candidate.as_str()), Some(fixture.base.as_str()), None] {
                let results = serde_json::json!({
                    "candidate_commit": commit,
                    "commands": [],
                    "tests": [{"id": id, "test_harness": "project checks", "status": "pass"}],
                    "summary": {"total": 1, "pass": 1, "fail": 0, "skipped": 0},
                    "error": null
                });
                fs::write(&tester, serde_json::to_vec(&results).unwrap()).unwrap();
                let contract = behavior_contract(reference);
                let mut request =
                    fixture.request(&candidate, Some(&contract), Some(&tester), &artifact);
                request.phase = CandidateGatePhase::FinalTester;
                let evidence = run_candidate_gate(request).unwrap();
                let bound = commit == Some(candidate.as_str());
                assert_eq!(
                    check(&evidence, "tester-reference-evidence").status,
                    if bound {
                        CandidateCheckStatus::Passed
                    } else {
                        CandidateCheckStatus::Error
                    },
                    "{evidence:#?}"
                );
                assert_eq!(
                    evidence.disposition,
                    if bound {
                        CandidateGateDisposition::Passed
                    } else {
                        CandidateGateDisposition::Blocked
                    }
                );
            }
        }
    }

    #[test]
    fn validates_parenthesized_source_path_test_identity() {
        let fixture = Fixture::new();
        let selector = "drain_is_idempotent";
        let candidate = fixture.commit(
            "src/lib.rs",
            &format!("#[test]\nfn {selector}() {{}}\n"),
            "Add drain lifecycle\n\n- Cover idempotent drain",
        );
        let tester = fixture.artifacts.path().join("tester-results.json");
        let results = serde_json::json!({
            "candidate_commit": candidate,
            "commands": [],
            "tests": [{
                "id": format!("src/lib.rs ({selector})"),
                "test_harness": "cargo-nextest",
                "status": "pass",
                "duration_ms": 1,
                "failure_excerpt": null
            }],
            "summary": {"total": 1, "pass": 1, "fail": 0, "skipped": 0},
            "error": null
        });
        fs::write(&tester, serde_json::to_vec(&results).unwrap()).unwrap();
        let artifact = fixture.artifacts.path().join("candidate-gate.json");
        let contract = behavior_contract(&format!("src/lib.rs ({selector})"));

        let evidence = run_candidate_gate(fixture.request(
            &candidate,
            Some(&contract),
            Some(&tester),
            &artifact,
        ))
        .unwrap();

        assert_eq!(evidence.disposition, CandidateGateDisposition::Passed);
        assert_eq!(
            check(&evidence, "tester-reference-evidence").status,
            CandidateCheckStatus::Passed
        );
    }

    #[test]
    fn python_command_reference_matches_exact_passing_tester_command() {
        let evidence = explicit_gate(
            "python3 tests/native.py",
            serde_json::json!([]),
            serde_json::json!([{
                "command":"python3 tests/native.py", "test_harness":"shell-harness", "exit_code":0,
                "duration_ms":1, "stdout_log":"commands/0-stdout.log", "stderr_log":"commands/0-stderr.log"
            }]),
        );
        assert_eq!(
            evidence.disposition,
            CandidateGateDisposition::Passed,
            "{evidence:#?}"
        );
    }

    #[test]
    fn python_command_reference_rejects_unbound_host_command_claim() {
        let fixture = Fixture::new();
        let candidate = fixture.commit(
            "tests/fluent_merge_readiness.py",
            "#!/usr/bin/env python3\n",
            "Add readiness journey\n\n- Cover the public workflow",
        );
        let tester = fixture.artifacts.path().join("tester-results.json");
        let results = serde_json::json!({
            "candidate_commit": candidate,
            "commands": [{
                "command": "python3 tests/fluent_merge_readiness.py",
                "test_harness": "shell-harness",
                "exit_code": 1,
                "duration_ms": 1,
                "stdout_log": "commands/0-stdout.log",
                "stderr_log": "commands/0-stderr.log"
            }],
            "tests": [],
            "summary": {"total": 0, "pass": 0, "fail": 0, "skipped": 0},
            "error": null
        });
        fs::write(&tester, serde_json::to_vec(&results).unwrap()).unwrap();
        let artifact = fixture.artifacts.path().join("candidate-gate.json");
        let contract = behavior_contract("python3 tests/fluent_merge_readiness.py");
        let accepted = vec!["python3 tests/fluent_merge_readiness.py".to_string()];
        let mut request = fixture.request(&candidate, Some(&contract), Some(&tester), &artifact);
        request.accepted_tester_commands = &accepted;

        let evidence = run_candidate_gate(request).unwrap();

        assert_eq!(evidence.disposition, CandidateGateDisposition::Rejected);
        assert_eq!(
            check(&evidence, "tester-reference-evidence").status,
            CandidateCheckStatus::Failed
        );
    }

    #[test]
    fn nextest_binary_and_test_identity_matches_exact_selector() {
        let fixture = Fixture::new();
        let candidate = fixture.commit(
            "tests/dashboard.rs",
            "#[test]\nfn preserves_selection() {}\n",
            "Add dashboard test\n\n- Cover dashboard selection",
        );
        let tester = fixture.artifacts.path().join("tester-results.json");
        let results = serde_json::json!({
            "candidate_commit": candidate,
            "commands": [],
            "tests": [{
                "id": "dashboard-tests::dashboard preserves_selection",
                "test_harness": "cargo-nextest",
                "status": "pass",
                "duration_ms": 1,
                "failure_excerpt": null
            }],
            "summary": {"total": 1, "pass": 1, "fail": 0, "skipped": 0},
            "error": null
        });
        fs::write(&tester, serde_json::to_vec(&results).unwrap()).unwrap();
        let artifact = fixture.artifacts.path().join("candidate-gate.json");
        let contract = behavior_contract("tests/dashboard.rs (preserves_selection)");

        let evidence = run_candidate_gate(fixture.request(
            &candidate,
            Some(&contract),
            Some(&tester),
            &artifact,
        ))
        .unwrap();

        assert_eq!(evidence.disposition, CandidateGateDisposition::Passed);
    }

    #[test]
    fn python_command_reference_rejects_different_passing_tester_command() {
        let fixture = Fixture::new();
        let candidate = fixture.commit(
            "tests/fluent_merge_readiness.py",
            "#!/usr/bin/env python3\n",
            "Add readiness journey\n\n- Cover the public workflow",
        );
        let tester = fixture.artifacts.path().join("tester-results.json");
        let results = serde_json::json!({
            "candidate_commit": candidate,
            "commands": [{
                "command": "python3 tests/a_different_journey.py",
                "test_harness": "shell-harness",
                "exit_code": 0,
                "duration_ms": 1,
                "stdout_log": "commands/0-stdout.log",
                "stderr_log": "commands/0-stderr.log"
            }],
            "tests": [],
            "summary": {"total": 0, "pass": 0, "fail": 0, "skipped": 0},
            "error": null
        });
        fs::write(&tester, serde_json::to_vec(&results).unwrap()).unwrap();
        let artifact = fixture.artifacts.path().join("candidate-gate.json");
        let contract = behavior_contract("python3 tests/fluent_merge_readiness.py");

        let evidence = run_candidate_gate(fixture.request(
            &candidate,
            Some(&contract),
            Some(&tester),
            &artifact,
        ))
        .unwrap();

        assert_eq!(
            check(&evidence, "tester-reference-evidence").diagnostics,
            vec![
                "no passing Tester result matches python3 tests/fluent_merge_readiness.py"
                    .to_string()
            ]
        );
    }

    #[test]
    fn missing_test_reference_reports_failed_tester_command_evidence() {
        let fixture = Fixture::new();
        let candidate = fixture.commit(
            "tests/dashboard.rs",
            "#[test]\nfn preserves_selection() {}\n",
            "Add dashboard test",
        );
        let tester = fixture.artifacts.path().join("tester-results.json");
        let results = serde_json::json!({
            "candidate_commit": candidate,
            "commands": [{
                "command": "cargo nextest run --test dashboard",
                "test_harness": "cargo-nextest",
                "exit_code": 101,
                "duration_ms": 1,
                "stdout_log": "commands/0-stdout.log",
                "stderr_log": "commands/0-stderr.log"
            }],
            "tests": [],
            "summary": {"total": 0, "pass": 0, "fail": 0, "skipped": 0},
            "error": null
        });
        fs::write(&tester, serde_json::to_vec(&results).unwrap()).unwrap();
        let artifact = fixture.artifacts.path().join("candidate-gate.json");
        let contract = behavior_contract("tests/dashboard.rs (preserves_selection)");

        let evidence = run_candidate_gate(fixture.request(
            &candidate,
            Some(&contract),
            Some(&tester),
            &artifact,
        ))
        .unwrap();

        let tester_check = check(&evidence, "tester-reference-evidence");
        assert_eq!(tester_check.status, CandidateCheckStatus::Failed);
        assert_eq!(
            tester_check.diagnostics,
            vec![
                "Tester command 0 exited 101: cargo nextest run --test dashboard".to_string(),
                format!(
                    "Tester command 0 stdout log: {}",
                    fixture
                        .artifacts
                        .path()
                        .join("commands/0-stdout.log")
                        .display()
                ),
                format!(
                    "Tester command 0 stderr log: {}",
                    fixture
                        .artifacts
                        .path()
                        .join("commands/0-stderr.log")
                        .display()
                ),
                "no passing Tester result matches tests/dashboard.rs (preserves_selection)".to_string(),
                "observed Tester identities for tests/dashboard.rs (preserves_selection) (showing 0 of 0): ".to_string(),
            ]
        );
    }

    #[test]
    fn validates_documented_double_colon_test_references() {
        let fixture = Fixture::new();
        let selector = "concurrent_registry_mutations_preserve_every_successful_change";
        let candidate = fixture.commit(
            "tests/scheduler_service.rs",
            &format!("#[test]\nfn {selector}() {{}}\n"),
            "Test concurrent registry unregistration\n\n- Cover registry updates",
        );
        let tester = fixture.artifacts.path().join("tester-results.json");
        let results = serde_json::json!({
            "candidate_commit": candidate,
            "commands": [],
            "tests": [{
                "id": format!("fluent::scheduler_service${selector}"),
                "test_harness": "cargo-nextest",
                "status": "pass",
                "duration_ms": 1,
                "failure_excerpt": null
            }],
            "summary": {"total": 1, "pass": 1, "fail": 0, "skipped": 0},
            "error": null
        });
        fs::write(&tester, serde_json::to_vec(&results).unwrap()).unwrap();
        let artifact = fixture.artifacts.path().join("candidate-gate.json");
        let contract = behavior_contract(&format!("tests/scheduler_service.rs::{selector}"));

        let evidence = run_candidate_gate(fixture.request(
            &candidate,
            Some(&contract),
            Some(&tester),
            &artifact,
        ))
        .unwrap();

        assert_eq!(evidence.disposition, CandidateGateDisposition::Passed);
        assert_eq!(
            check(&evidence, "behavior-references").status,
            CandidateCheckStatus::Passed
        );
        assert_eq!(
            check(&evidence, "tester-reference-evidence").status,
            CandidateCheckStatus::Passed
        );
    }

    #[test]
    fn validates_nextest_integration_test_references() {
        let fixture = Fixture::new();
        let selector = "skills_add_replaces_stale_shim_installation";
        let candidate = fixture.commit(
            "tests/binary.rs",
            &format!("#[test]\nfn {selector}() {{}}\n"),
            "Add skills test\n\n- Cover skill installation",
        );
        let tester = fixture.artifacts.path().join("tester-results.json");
        let results = serde_json::json!({
            "candidate_commit": candidate,
            "commands": [],
            "tests": [{
                "id": format!("fluent::binary${selector}"),
                "test_harness": "cargo-nextest",
                "status": "pass",
                "duration_ms": 1,
                "failure_excerpt": null
            }],
            "summary": {"total": 1, "pass": 1, "fail": 0, "skipped": 0},
            "error": null
        });
        fs::write(&tester, serde_json::to_vec(&results).unwrap()).unwrap();
        let artifact = fixture.artifacts.path().join("candidate-gate.json");
        let contract = behavior_contract(&format!("tests/binary.rs ({selector})"));

        let evidence = run_candidate_gate(fixture.request(
            &candidate,
            Some(&contract),
            Some(&tester),
            &artifact,
        ))
        .unwrap();

        assert_eq!(evidence.disposition, CandidateGateDisposition::Passed);
        assert_eq!(
            check(&evidence, "tester-reference-evidence").status,
            CandidateCheckStatus::Passed
        );
    }

    #[test]
    fn matches_shell_script_without_description_literals() {
        let fixture = Fixture::new();
        let candidate = fixture.commit(
            "tests/lifecycle.sh",
            "#!/bin/sh\nset -eu\nexit 0\n",
            "Add lifecycle script\n\n- Exercise lifecycle commands",
        );
        let contract = behavior_contract("tests/lifecycle.sh (help documents drain lifecycle)");
        let tester = fixture.artifacts.path().join("tester-results.json");
        let artifact = fixture.artifacts.path().join("candidate-gate.json");
        let source_gate =
            run_candidate_gate(fixture.request(&candidate, Some(&contract), None, &artifact))
                .unwrap();
        assert_eq!(
            source_gate.disposition,
            CandidateGateDisposition::Passed,
            "{source_gate:#?}"
        );
        assert_eq!(
            check(&source_gate, "tester-reference-evidence").status,
            CandidateCheckStatus::Skipped
        );

        for harness in ["shell-harness", "project lifecycle checks"] {
            for (path, status, accepted) in [
                ("tests/lifecycle.sh", "pass", true),
                ("tests/other.sh", "pass", false),
                ("lifecycle.sh", "pass", false),
                ("tests/./lifecycle.sh", "pass", false),
                (
                    "tests/lifecycle.sh (help documents drain lifecycle)",
                    "pass",
                    false,
                ),
                ("help documents drain lifecycle", "pass", false),
                ("tests/lifecycle.sh", "fail", false),
                ("tests/lifecycle.sh", "skipped", false),
            ] {
                let results = serde_json::json!({
                    "candidate_commit": candidate,
                    "commands": [],
                    "tests": [{"id": path, "test_harness": harness, "status": status}],
                    "summary": {"total": 1, "pass": usize::from(status == "pass"), "fail": usize::from(status == "fail"), "skipped": usize::from(status == "skipped")},
                    "error": null
                });
                fs::write(&tester, serde_json::to_vec(&results).unwrap()).unwrap();
                let mut request =
                    fixture.request(&candidate, Some(&contract), Some(&tester), &artifact);
                request.phase = CandidateGatePhase::FinalTester;
                let evidence = run_candidate_gate(request).unwrap();
                assert_eq!(
                    check(&evidence, "behavior-references").status,
                    CandidateCheckStatus::Passed
                );
                assert_eq!(
                    evidence.disposition,
                    if accepted {
                        CandidateGateDisposition::Passed
                    } else {
                        CandidateGateDisposition::Rejected
                    },
                    "{harness}, {path}, {status}: {evidence:#?}"
                );
                let preserved: serde_json::Value =
                    serde_json::from_slice(&fs::read(&tester).unwrap()).unwrap();
                assert_eq!(preserved, results);
            }
        }

        // A passing path-shaped result cannot replace a committed script.
        let missing = behavior_contract("tests/missing.sh (help documents drain lifecycle)");
        let mut results = serde_json::json!({
            "candidate_commit": candidate,
            "commands": [],
            "tests": [{"id": "tests/missing.sh", "test_harness": "shell-harness", "status": "pass"}],
            "summary": {"total": 1, "pass": 1, "fail": 0, "skipped": 0},
            "error": null
        });
        fs::write(&tester, serde_json::to_vec(&results).unwrap()).unwrap();
        let evidence = run_candidate_gate(fixture.request(
            &candidate,
            Some(&missing),
            Some(&tester),
            &artifact,
        ))
        .unwrap();
        assert_eq!(
            check(&evidence, "behavior-references").status,
            CandidateCheckStatus::Failed
        );
        assert!(
            check(&evidence, "behavior-references")
                .diagnostics
                .iter()
                .any(|line| line.contains("missing committed test path tests/missing.sh"))
        );

        // The display label cannot make malformed structured evidence valid.
        results["tests"][0]["id"] = "tests/lifecycle.sh".into();
        results["tests"][0]
            .as_object_mut()
            .unwrap()
            .remove("status");
        fs::write(&tester, serde_json::to_vec(&results).unwrap()).unwrap();
        let evidence = run_candidate_gate(fixture.request(
            &candidate,
            Some(&contract),
            Some(&tester),
            &artifact,
        ))
        .unwrap();
        assert_eq!(evidence.disposition, CandidateGateDisposition::Blocked);
        assert_eq!(
            check(&evidence, "tester-reference-evidence").status,
            CandidateCheckStatus::Error
        );
        assert!(
            check(&evidence, "tester-reference-evidence")
                .diagnostics
                .iter()
                .any(|line| line.contains("cannot parse"))
        );
    }

    #[test]
    fn shell_evidence_uses_the_harness_native_script_identity() {
        let fixture = Fixture::new();
        let candidate = fixture.commit(
            "tests/behaviors/operations/test-dashboard.sh",
            "#!/bin/sh\n# preserves selection\n",
            "Add dashboard journey\n\n- Cover dashboard selection",
        );
        let tester = fixture.artifacts.path().join("tester-results.json");
        fs::write(
            &tester,
            format!(
                "{{\"candidate_commit\":\"{candidate}\",\"commands\":[],\"tests\":[{{\"id\":\"tests/behaviors/operations/test-dashboard.sh\",\"test_harness\":\"shell-harness\",\"status\":\"pass\",\"duration_ms\":null,\"failure_excerpt\":null}}],\"summary\":{{\"total\":1,\"pass\":1,\"fail\":0,\"skipped\":0}},\"error\":null}}"
            ),
        )
        .unwrap();
        let artifact = fixture.artifacts.path().join("candidate-gate.json");
        let contract =
            behavior_contract("tests/behaviors/operations/test-dashboard.sh (preserves selection)");

        let evidence = run_candidate_gate(fixture.request(
            &candidate,
            Some(&contract),
            Some(&tester),
            &artifact,
        ))
        .unwrap();

        assert_eq!(evidence.disposition, CandidateGateDisposition::Passed);
    }

    #[test]
    fn malformed_structured_tester_evidence_blocks_instead_of_rejecting_candidate() {
        let fixture = Fixture::new();
        let candidate = fixture.commit(
            "tests/dashboard.rs",
            "#[test]\nfn preserves_selection() {}\n",
            "Add dashboard test",
        );
        let tester = fixture.artifacts.path().join("tester-results.json");
        fs::write(&tester, "not json\n").unwrap();
        let artifact = fixture.artifacts.path().join("candidate-gate.json");
        let contract = behavior_contract("tests/dashboard.rs (preserves_selection)");

        let evidence = run_candidate_gate(fixture.request(
            &candidate,
            Some(&contract),
            Some(&tester),
            &artifact,
        ))
        .unwrap();

        assert_eq!(evidence.disposition, CandidateGateDisposition::Blocked);
        assert_eq!(
            check(&evidence, "tester-reference-evidence").status,
            CandidateCheckStatus::Error
        );
    }

    #[test]
    fn project_check_failure_rejects_candidate_and_keeps_its_log() {
        let fixture = Fixture::new();
        let candidate = fixture.commit("src/lib.rs", "pub fn value() {}\n", "Add value");
        let hooks = fixture.project.path().join(".fluent/hooks");
        fs::create_dir_all(&hooks).unwrap();
        let hook = hooks.join("check-pre-merge");
        fs::write(
            &hook,
            "#!/bin/sh\nprintf 'dependency cycle: app -> render -> app\\n'\nexit 9\n",
        )
        .unwrap();
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
        let artifact = fixture.artifacts.path().join("candidate-gate.json");

        let evidence =
            run_candidate_gate(fixture.request(&candidate, None, None, &artifact)).unwrap();

        assert_eq!(evidence.disposition, CandidateGateDisposition::Rejected);
        let project = check(&evidence, "project-check");
        assert_eq!(project.status, CandidateCheckStatus::Failed);
        assert!(project.log_path.as_ref().is_some_and(|path| {
            fs::read_to_string(Path::new(path))
                .unwrap()
                .contains("dependency cycle")
        }));
    }

    #[test]
    fn project_check_receives_the_effective_candidate_base() {
        let fixture = Fixture::new();
        let candidate = fixture.commit("src/lib.rs", "pub fn value() {}\n", "Add value");
        let hooks = fixture.project.path().join(".fluent/hooks");
        fs::create_dir_all(&hooks).unwrap();
        let hook = hooks.join("check-pre-merge");
        fs::write(
            &hook,
            format!(
                "#!/bin/sh\n[ \"$FLUENT_BASE_COMMIT\" = \"{}\" ] || exit 9\n",
                fixture.base
            ),
        )
        .unwrap();
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
        let artifact = fixture.artifacts.path().join("candidate-gate.json");

        let evidence =
            run_candidate_gate(fixture.request(&candidate, None, None, &artifact)).unwrap();

        assert_eq!(evidence.disposition, CandidateGateDisposition::Passed);
        assert_eq!(
            check(&evidence, "project-check").status,
            CandidateCheckStatus::Passed
        );
    }

    #[test]
    fn a_project_check_that_mutates_the_candidate_blocks_as_invalid_configuration() {
        let fixture = Fixture::new();
        let candidate = fixture.commit("src/lib.rs", "pub fn value() {}\n", "Add value");
        let hooks = fixture.project.path().join(".fluent/hooks");
        fs::create_dir_all(&hooks).unwrap();
        let hook = hooks.join("check-pre-merge");
        fs::write(&hook, "#!/bin/sh\nprintf dirty > hook-output.txt\nexit 0\n").unwrap();
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
        let artifact = fixture.artifacts.path().join("candidate-gate.json");

        let evidence =
            run_candidate_gate(fixture.request(&candidate, None, None, &artifact)).unwrap();

        assert_eq!(evidence.disposition, CandidateGateDisposition::Blocked);
        assert_eq!(
            check(&evidence, "project-check").status,
            CandidateCheckStatus::Error
        );
    }

    fn rejected_reference_gate(diagnostics: &[&str]) -> CandidateGateEvidence {
        CandidateGateEvidence {
            schema_version: CANDIDATE_GATE_SCHEMA_VERSION,
            phase: CandidateGatePhase::FinalTester,
            disposition: CandidateGateDisposition::Rejected,
            base_commit: "base".to_string(),
            candidate_commit: "candidate".to_string(),
            tester_results_path: Some("tester-results.json".to_string()),
            metadata_rewrite: None,
            checks: vec![CandidateCheckEvidence {
                name: "tester-reference-evidence".to_string(),
                status: CandidateCheckStatus::Failed,
                diagnostics: diagnostics
                    .iter()
                    .map(|diagnostic| diagnostic.to_string())
                    .collect(),
                log_path: None,
            }],
        }
    }

    #[test]
    fn rejected_gate_requests_one_exact_command_for_host_evidence() {
        let evidence = rejected_reference_gate(&[
            "no passing Tester result matches python3 tests/fluent_merge_readiness.py",
            "no passing Tester result matches python3 tests/fluent_merge_readiness.py",
            "no passing Tester result matches tests/readiness.py::current_frontier",
        ]);

        assert_eq!(
            requested_host_evidence_command(&evidence).as_deref(),
            Some("python3 tests/fluent_merge_readiness.py")
        );
    }

    #[test]
    fn rejected_gate_does_not_request_ambiguous_or_unrelated_host_evidence() {
        let multiple_commands = rejected_reference_gate(&[
            "no passing Tester result matches python3 tests/one.py",
            "no passing Tester result matches python3 tests/two.py",
        ]);
        assert_eq!(requested_host_evidence_command(&multiple_commands), None);

        let mut unrelated_failure =
            rejected_reference_gate(&["no passing Tester result matches python3 tests/one.py"]);
        unrelated_failure.checks.push(CandidateCheckEvidence {
            name: "candidate-identity".to_string(),
            status: CandidateCheckStatus::Failed,
            diagnostics: vec!["candidate is dirty".to_string()],
            log_path: None,
        });
        assert_eq!(requested_host_evidence_command(&unrelated_failure), None);

        let selector_only = rejected_reference_gate(&[
            "no passing Tester result matches tests/readiness.py::current_frontier",
        ]);
        assert_eq!(requested_host_evidence_command(&selector_only), None);
    }

    fn explicit_gate(
        reference: &str,
        tests: serde_json::Value,
        commands: serde_json::Value,
    ) -> CandidateGateEvidence {
        let fixture = Fixture::new();
        fixture.commit("tests/native.sh", "#!/bin/sh\n", "Add native script");
        fixture.commit("src/native.rs", "fn works() {}\n", "Add Rust test");
        fixture.commit(
            "tests/native.py",
            "class Native:\n def works(self): pass\n",
            "Add Python test",
        );
        fixture.commit(
            ".fluent/extract-tester-results",
            "#!/bin/sh\nprintf '[]\\n'\n",
            "Add extractor",
        );
        let configured = if commands.as_array().unwrap().is_empty() {
            serde_json::json!([
                {"command":"check", "test_harness":"shell-harness"}, {"command":"check", "test_harness":"shell-harness"}
            ])
        } else {
            serde_json::Value::Array(commands.as_array().unwrap().iter().map(|command| serde_json::json!({"command":command["command"], "test_harness":command["test_harness"]})).collect())
        };
        let candidate = fixture.commit(
            ".fluent/tester.yaml",
            &serde_json::json!({"commands":configured}).to_string(),
            "Add configured checks",
        );
        let mut tester = fixture.artifacts.path().join("tester-results.json");
        let values = tests.as_array().unwrap();
        let results = serde_json::json!({"candidate_commit": candidate, "commands": commands, "tests": tests,
            "summary":{"total":values.len(),"pass":values.iter().filter(|test| test["status"]=="pass").count(),"fail":values.iter().filter(|test| test["status"]=="fail").count(),"skipped":values.iter().filter(|test| test["status"]=="skipped").count()},"error":null});
        if !commands.as_array().unwrap().is_empty() {
            use crate::work_model::{TaskKind, TaskOutput, TaskStatus, WorkItem, WorkModelStore};
            let mut item = WorkItem::planned("work-1", "Check explicit commands");
            item.add_initial_attempt("attempt-1").unwrap();
            let writer = &mut item.attempts[0].tasks[0];
            writer.status = TaskStatus::Complete;
            writer.output = Some(TaskOutput {
                workspace_id: writer.workspace_access.writes[0].id.clone(),
                workspace_path: fixture.root.path().display().to_string(),
                source_branch: "candidate".into(),
                base_commit: Some(fixture.base.clone()),
                commit: candidate.clone(),
                no_change: None,
                learner_canonicalization: None,
            });
            item.add_next_review_tasks("attempt-1", &["tests"]).unwrap();
            for task in &mut item.attempts[0].tasks {
                if task.kind == TaskKind::Review {
                    task.status = TaskStatus::Complete;
                }
            }
            let id = item.add_final_tester_task("attempt-1").unwrap();
            let task = item.attempts[0]
                .tasks
                .iter_mut()
                .find(|task| task.id == id)
                .unwrap();
            task.status = TaskStatus::Complete;
            let area = fixture
                .project
                .path()
                .join(&task.artifact_area.as_ref().unwrap().path);
            fs::create_dir_all(area.join("commands")).unwrap();
            for command in commands.as_array().unwrap() {
                for log in ["stdout_log", "stderr_log"] {
                    fs::write(area.join(command[log].as_str().unwrap()), "").unwrap();
                }
            }
            tester = area.join("tester-results.json");
            WorkModelStore::new(fixture.project.path())
                .create_work_item(&item)
                .unwrap();
        }
        fs::write(&tester, serde_json::to_vec(&results).unwrap()).unwrap();
        let contract = behavior_contract(reference);
        run_candidate_gate(fixture.request(
            &candidate,
            Some(&contract),
            Some(&tester),
            &fixture.artifacts.path().join("candidate-gate.json"),
        ))
        .unwrap()
    }

    #[test]
    fn command_reference_requires_readable_current_bindings() {
        let (root, item) = crate::recovery_contracts::verification_tests::fixture(false);
        let attempt = &item.attempts[0];
        let output = attempt.tasks[0].output.as_ref().unwrap();
        let result = crate::verification_assessment::assess_canonical_checks(root.path(), attempt)
            .unwrap()
            .results_path;
        let mut results: crate::tester::TesterResults =
            serde_json::from_slice(&fs::read(&result).unwrap()).unwrap();
        results.commands[0].exit_code = 0;
        fs::write(&result, serde_json::to_vec(&results).unwrap()).unwrap();
        let contract =
            behavior_contract(r#"{"kind":"command","command":"false","command_index":0}"#);
        let artifact = root.path().join("gate.json");
        let mut request = CandidateGateRequest {
            project_root: root.path(),
            workspace: root.path(),
            work_item_id: &item.id,
            attempt_id: &attempt.id,
            task_id: Some(&attempt.tasks[0].id),
            phase: CandidateGatePhase::Writer,
            base_commit: &output.commit,
            candidate_commit: &output.commit,
            completion_contract: Some(&contract),
            allow_tree_identical_metadata_rewrite: false,
            metadata_rewrite_authorization: None,
            tester_results_path: Some(&result),
            accepted_tester_commands: &[],
            artifact_path: &artifact,
        };
        let store = crate::work_model::WorkModelStore::new(root.path());
        store.create_work_item(&item).unwrap();
        assert!(
            check_behavior_references(&request)
                .unwrap()
                .iter()
                .all(|check| check.status == CandidateCheckStatus::Passed)
        );
        let copy = root.path().join("unbound-results.json");
        fs::write(&copy, fs::read(&result).unwrap()).unwrap();
        request.tester_results_path = Some(&copy);
        assert!(
            check_behavior_references(&request)
                .unwrap()
                .iter()
                .any(|check| check.status != CandidateCheckStatus::Passed)
        );
        request.tester_results_path = Some(&result);
        let child = store.work_attempt_path(&item.id, &attempt.id).unwrap();
        let child_bytes = fs::read(&child).unwrap();
        fs::remove_file(&child).unwrap();
        assert!(
            check_behavior_references(&request)
                .unwrap()
                .iter()
                .any(|check| check.status != CandidateCheckStatus::Passed)
        );
        fs::write(child, child_bytes).unwrap();
        let saved = store.work_item_path(&item.id).unwrap();
        let bytes = fs::read(&saved).unwrap();
        fs::write(&saved, b"{broken snapshot").unwrap();
        assert!(
            check_behavior_references(&request)
                .unwrap()
                .iter()
                .any(|check| check.status != CandidateCheckStatus::Passed)
        );
        fs::remove_file(&saved).unwrap();
        assert!(
            check_behavior_references(&request)
                .unwrap()
                .iter()
                .any(|check| check.status != CandidateCheckStatus::Passed)
        );
        fs::write(saved, bytes).unwrap();
    }

    #[test]
    fn explicit_reference_meaning_controls_evidence_matching() {
        let tests = serde_json::json!([
            {"id":"tests/native.sh", "test_harness":"display", "status":"pass"},
            {"id":"tests/native.sh::works", "test_harness":"display", "status":"pass"}
        ]);
        for reference in [
            r#"{"kind":"whole-script","path":"tests/native.sh","description":"not a native identity"}"#,
            r#"{"kind":"named-test","path":"tests/native.sh","name":"works","description":"another explanation"}"#,
        ] {
            assert_eq!(
                explicit_gate(reference, tests.clone(), serde_json::json!([])).disposition,
                CandidateGateDisposition::Passed
            );
        }
        let commands = serde_json::json!([
            {"command":"check","test_harness":"shell-harness","exit_code":1,"duration_ms":1,"stdout_log":"commands/0-stdout.log","stderr_log":"commands/0-stderr.log"},
            {"command":"check","test_harness":"shell-harness","exit_code":0,"duration_ms":1,"stdout_log":"commands/1-stdout.log","stderr_log":"commands/1-stderr.log"}
        ]);
        for (reference, expected) in [
            (
                r#"{"kind":"command","command":"check","command_index":1,"description":"the second occurrence"}"#,
                CandidateGateDisposition::Passed,
            ),
            (
                r#"{"kind":"command","command":"check"}"#,
                CandidateGateDisposition::Rejected,
            ),
            (
                r#"{"kind":"command","command":"check","command_index":0}"#,
                CandidateGateDisposition::Rejected,
            ),
            (
                r#"{"kind":"command","command":"check","command_index":2}"#,
                CandidateGateDisposition::Rejected,
            ),
        ] {
            assert_eq!(
                explicit_gate(reference, serde_json::json!([]), commands.clone()).disposition,
                expected,
                "{reference}"
            );
        }
        for malformed in [
            r#"{"kind":"named-test","path":"tests/native.sh","name":"works","binary":"pkg"}"#,
            r#"{"kind":"named-test","path":"tests/native.sh","name":""}"#,
            r#"{"kind":"whole-script","path":"tests/native.sh","name":"works"}"#,
            r#"{"kind":"named-test","path":"tests//native.sh","name":"works"}"#,
            r#"{"kind":"named-test","path":"src/native.rs","name":"other.rs::works"}"#,
            r#"{"kind":"unknown","path":"tests/native.sh"}"#,
            r#"{"kind":"command","command":"check","command_index":-1}"#,
        ] {
            assert_ne!(
                explicit_gate(malformed, tests.clone(), commands.clone()).disposition,
                CandidateGateDisposition::Passed,
                "{malformed}"
            );
        }
    }

    #[test]
    fn named_shell_reference_requires_exact_script_and_test() {
        let reference = r#"{"kind":"named-test","path":"tests/native.sh","name":"works"}"#;
        for (id, expected) in [
            ("tests/native.sh::works", true),
            ("tests/other.sh::works", false),
            ("tests/native.sh::other", false),
            ("native.sh::works", false),
        ] {
            let evidence = explicit_gate(
                reference,
                serde_json::json!([{"id":id,"status":"pass","test_harness":"shell-harness"}]),
                serde_json::json!([]),
            );
            assert_eq!(
                evidence.disposition == CandidateGateDisposition::Passed,
                expected,
                "{id}"
            );
        }
        // A producer may separate its display label from its typed native identity.
        let evidence = explicit_gate(
            reference,
            serde_json::json!([{"id":"human readable label", "status":"pass","test_harness":"shell-harness","identity":{"kind":"named-test","path":"tests/native.sh","name":"works"}}]),
            serde_json::json!([]),
        );
        assert_eq!(evidence.disposition, CandidateGateDisposition::Passed);
    }

    #[test]
    fn named_shell_reference_rejects_whole_script_and_conflicting_results() {
        let reference = r#"{"kind":"named-test","path":"tests/native.sh","name":"works"}"#;
        for tests in [
            serde_json::json!([{"id":"tests/native.sh","status":"pass","test_harness":"shell-harness"}]),
            serde_json::json!([{"id":"tests/native.sh::works","status":"fail","test_harness":"shell-harness"}]),
            serde_json::json!([{"id":"tests/native.sh::works","status":"skipped","test_harness":"shell-harness"}]),
            serde_json::json!([{"id":"tests/native.sh::works","status":"pass","test_harness":"shell-harness"},{"id":"tests/native.sh::works","status":"fail","test_harness":"shell-harness"}]),
            serde_json::json!([{"id":"tests/native.sh::works","status":"pass","test_harness":"shell-harness","identity":{"kind":"named-test","path":"tests/other.sh","name":"works"}}]),
        ] {
            assert_ne!(
                explicit_gate(reference, tests, serde_json::json!([])).disposition,
                CandidateGateDisposition::Passed
            );
        }
    }

    #[test]
    fn partial_structured_identities_cannot_erase_native_qualifiers() {
        for (reference, raw, identity) in [
            (
                "src/native.rs (pkg::lib$module::works)",
                "src/other.rs (pkg::lib$module::works)",
                serde_json::json!({"kind":"rust","name":"works"}),
            ),
            (
                "src/native.rs (pkg::lib$module::works)",
                "src/native.rs (other::lib$module::works)",
                serde_json::json!({"kind":"rust","name":"works"}),
            ),
            (
                "src/native.rs (pkg::lib$module::works)",
                "src/native.rs (pkg::lib$other::works)",
                serde_json::json!({"kind":"rust","name":"works"}),
            ),
            (
                "tests/native.py (Native.works)",
                "tests/other.py::module.Native.works",
                serde_json::json!({"kind":"python","name":"works"}),
            ),
            (
                "tests/native.py (Native.works)",
                "tests/native.py::module.Other.works",
                serde_json::json!({"kind":"python","name":"works"}),
            ),
            (
                "src/native.rs (other::lib$other::works)",
                "src/native.rs (pkg::lib$module::works)",
                serde_json::json!({"kind":"python","name":"works"}),
            ),
            (
                "src/native.rs (pkg::lib$module::works)",
                "tests/native.py::module.Native.works",
                serde_json::json!({"kind":"rust","name":"works"}),
            ),
            (
                "tests/native.py (Native.works)",
                "src/native.rs (pkg::lib$module::works)",
                serde_json::json!({"kind":"python","path":"tests/native.py","class":"Native","name":"works"}),
            ),
            (
                "src/native.rs (pkg::lib$module::works)",
                "tests/other.sh::works",
                serde_json::json!({"kind":"rust","path":"src/native.rs","binary":"pkg::lib","name":"module::works"}),
            ),
            (
                "tests/native.py (Native.works)",
                "tests/other.sh::works",
                serde_json::json!({"kind":"python","name":"works"}),
            ),
        ] {
            let gate = explicit_gate(
                reference,
                serde_json::json!([{"id":raw,"status":"pass","test_harness":"native","identity":identity}]),
                serde_json::json!([]),
            );
            assert_ne!(gate.disposition, CandidateGateDisposition::Passed, "{raw}");
        }
    }

    #[test]
    fn unambiguous_legacy_reference_meaning_is_preserved() {
        for (reference, id) in [
            ("tests/native.sh (coverage description)", "tests/native.sh"),
            (
                "src/native.rs (pkg::lib$module::works)",
                "src/native.rs (pkg::lib$module::works)",
            ),
            ("tests/native.py (Native.works)", "works (module.Native)"),
            (
                r#"{"kind":"named-test","path":"src/native.rs","name":"module::works","binary":"pkg::lib"}"#,
                "src/native.rs (pkg::lib$module::works)",
            ),
        ] {
            assert_eq!(
                explicit_gate(
                    reference,
                    serde_json::json!([{"id":id,"status":"pass","test_harness":"native"}]),
                    serde_json::json!([])
                )
                .disposition,
                CandidateGateDisposition::Passed,
                "{reference}"
            );
        }
    }
}
