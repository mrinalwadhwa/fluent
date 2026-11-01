//! Assess current verification facts without scheduling or publishing state.
use crate::check_applicability::resolve_check_applicability;
use crate::work_model::{
    ArtifactRef, EvidenceDocument, EvidenceRecovery, EvidenceRecoveryState, EvidenceResult, Task,
    TaskKind, TaskStatus,
};
use crate::{review, work_model};
use anyhow::{Context, Result, bail};
use chrono::DateTime;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
/// Credit only errors produced by the candidate's configuration or extractor.
pub(crate) fn tester_error_is_candidate_defect(error: &crate::tester::TesterError) -> bool {
    matches!(
        error.kind.as_str(),
        "tester_yaml_problem"
            | "extractor_missing"
            | "extractor_failure"
            | "duplicate_test_ids"
            | "invalid_test_identity"
    )
}

pub(crate) fn landing_evidence_digest(
    project_root: &Path,
    checks: &[ArtifactRef],
    reviews: &[ArtifactRef],
) -> Result<String> {
    let mut hash = Sha256::new();
    for artifact in checks.iter().chain(reviews) {
        hash.update(artifact.producer_id.as_bytes());
        hash.update([0]);
        hash.update(artifact.path.as_bytes());
        hash.update([0]);
        let path = Path::new(&artifact.path);
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            project_root.join(path)
        };
        hash.update(
            fs::read(&path)
                .with_context(|| format!("read accepted landing evidence {}", path.display()))?,
        );
        hash.update([0]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

/// Verify retained Tester proof for the merged commit without changing workflow state.
pub(crate) enum LandedTesterSource {
    Attempt,
    MergeCandidate,
}

pub(crate) fn assess_landed_tester_history(
    root: &Path,
    attempt: &work_model::Attempt,
    candidate: &work_model::MergeCandidate,
    merged_commit: &str,
    source: LandedTesterSource,
) -> Result<String> {
    anyhow::ensure!(!merged_commit.is_empty(), "merged commit is missing");
    let tester = attempt.tasks.iter().rev().find(|task| {
        task.kind == TaskKind::Tester
            && task.status == TaskStatus::Complete
            && !task.is_failed_command_recheck()
            && task
                .review_context
                .as_ref()
                .is_some_and(|context| context.candidate_commit == merged_commit)
    });
    let landing = match source {
        LandedTesterSource::Attempt => None,
        LandedTesterSource::MergeCandidate => Some(
            candidate
                .merge_state
                .check_artifacts
                .iter()
                .rev()
                .find(|artifact| artifact.producer_id == "merge-tester")
                .ok_or_else(|| anyhow::anyhow!("merge Tester artifact is missing"))?,
        ),
    };
    let (path, baseline_path) = if let Some(landing) = landing {
        let prefix = format!(
            "{}/{}/{}/{}/merge/verification-",
            crate::work_model::WORK_ARTIFACTS_DIR,
            attempt.work_item_id,
            attempt.id,
            candidate.id
        );
        let attempt_path = tester
            .and_then(|task| task.artifact_area.as_ref())
            .map(|area| format!("{}/tester-results.json", area.path));
        let merge_dir = landing
            .path
            .strip_prefix(&prefix)
            .and_then(|rest| rest.strip_suffix("/tester/tester-results.json"));
        anyhow::ensure!(
            attempt_path.as_deref() == Some(landing.path.as_str())
                || merge_dir.is_some_and(|part| !part.is_empty() && !part.contains('/')),
            "merge Tester artifact differs from its recorded identity"
        );
        let baseline = candidate
            .merge_state
            .check_artifacts
            .iter()
            .find(|artifact| artifact.producer_id == "merge-target-baseline")
            .ok_or_else(|| anyhow::anyhow!("merge target baseline is missing"))?;
        if let Some(part) = merge_dir {
            anyhow::ensure!(
                baseline.path == format!("{prefix}{part}/target-baseline/tester-results.json"),
                "merge target baseline differs from its recorded identity"
            );
        }
        (landing.path.clone(), Some(baseline.path.as_str()))
    } else {
        let tester =
            tester.ok_or_else(|| anyhow::anyhow!("completed merged-commit Tester is missing"))?;
        let area = tester
            .artifact_area
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Tester artifact area is missing"))?;
        anyhow::ensure!(
            area.path
                == crate::work_model::work_artifact_path(
                    &attempt.work_item_id,
                    &attempt.id,
                    &tester.id
                ),
            "Tester artifact area differs from its recorded identity"
        );
        (format!("{}/tester-results.json", area.path), None)
    };
    let area = path
        .strip_suffix("/tester-results.json")
        .ok_or_else(|| anyhow::anyhow!("Tester artifact path is invalid"))?;
    let (_, bytes) = crate::verification_assessment::bound_recovery_artifact(root, &path)?;
    let results: crate::tester::TesterResults = serde_json::from_slice(&bytes)?;
    let manifest_path = format!("{area}/canonical-run.json");
    let (_, manifest_bytes) =
        crate::verification_assessment::bound_recovery_artifact(root, &manifest_path)?;
    let manifest: crate::tester::CanonicalRunManifest = serde_json::from_slice(&manifest_bytes)?;
    anyhow::ensure!(
        manifest.schema_version == 1
            && manifest.full_run
            && manifest.commands.len() == results.commands.len(),
        "retained Tester manifest does not describe a complete run"
    );
    for (name, digest) in [
        (".fluent/tester.yaml", &manifest.config_digest),
        (".fluent/extract-tester-results", &manifest.extractor_digest),
    ] {
        let object = crate::git::run_raw(root, &["show", &format!("{}:{name}", merged_commit)])?;
        anyhow::ensure!(
            object.status.success(),
            "merged check configuration is unavailable: {name}"
        );
        anyhow::ensure!(
            &format!("sha256:{:x}", Sha256::digest(&object.stdout)) == digest,
            "retained Tester configuration differs from the merged commit: {name}"
        );
        if name == ".fluent/tester.yaml" {
            let config: crate::tester::TesterConfig = serde_yaml::from_slice(&object.stdout)?;
            anyhow::ensure!(
                config.commands == manifest.commands,
                "retained Tester manifest differs from configured commands"
            );
        }
    }
    anyhow::ensure!(
        results.candidate_commit.as_deref() == Some(merged_commit),
        "Tester result does not match the merged candidate"
    );
    anyhow::ensure!(
        results.error.is_none()
            && results.commands.iter().all(|command| command
                .process_settlement
                .as_ref()
                .is_none_or(|settlement| settlement.leaks.is_empty())),
        "retained Tester result has a harness error or process leak"
    );
    if results.summary.fail != 0
        || results
            .commands
            .iter()
            .any(|command| command.exit_code != 0)
    {
        let baseline = baseline_path.ok_or_else(|| {
            anyhow::anyhow!("retained Tester result does not record a passing run")
        })?;
        let (_, bytes) = crate::verification_assessment::bound_recovery_artifact(root, baseline)?;
        let baseline_results: crate::tester::TesterResults = serde_json::from_slice(&bytes)?;
        anyhow::ensure!(
            baseline_results.error.is_none(),
            "retained target baseline has a harness error"
        );
        anyhow::ensure!(
            baseline_results.candidate_commit.as_deref()
                == candidate
                    .merge_state
                    .landing_intent
                    .as_ref()
                    .map(|intent| intent.target_before.as_str()),
            "retained target baseline does not match the landing target"
        );
        let native_match = crate::tester_evidence::introduced_tester_failures(
            &root.join(&path),
            Some(&root.join(baseline)),
        ) == 0
            && crate::tester_evidence::unexplained_failed_tester_commands(
                &root.join(&path),
                Some(&root.join(baseline)),
            ) == 0;
        if !native_match {
            anyhow::ensure!(
                candidate.merge_state.landing_intent.is_some()
                    && candidate
                        .merge_state
                        .check_artifacts
                        .iter()
                        .any(|artifact| {
                            artifact.producer_id == "merge-tester-manifest"
                                && artifact.path == manifest_path
                        }),
                "reviewed host proof requires the landing manifest bound in retained evidence"
            );
        }
        anyhow::ensure!(
            native_match
                || crate::tester_evidence::reviewed_host_proof_covers_expertise_only_merge(
                    root,
                    attempt,
                    merged_commit,
                    &root.join(&path),
                    Some(&root.join(baseline)),
                )?,
            "retained Tester result has new failures or unexplained failed commands"
        );
    }
    anyhow::ensure!(
        results.summary.total == results.tests.len()
            && results.summary.pass
                == results
                    .tests
                    .iter()
                    .filter(|test| test.status == "pass")
                    .count()
            && results.summary.fail
                == results
                    .tests
                    .iter()
                    .filter(|test| test.status == "fail")
                    .count()
            && results.summary.skipped
                == results
                    .tests
                    .iter()
                    .filter(|test| matches!(test.status.as_str(), "skipped" | "not_run"))
                    .count(),
        "retained Tester summary disagrees with its test results"
    );
    for (index, command) in results.commands.iter().enumerate() {
        anyhow::ensure!(
            command.command == manifest.commands[index].command
                && command.test_harness == manifest.commands[index].test_harness,
            "retained Tester command differs from its manifest"
        );
        for (actual, expected) in [
            (&command.stdout_log, format!("commands/{index}-stdout.log")),
            (&command.stderr_log, format!("commands/{index}-stderr.log")),
        ] {
            anyhow::ensure!(
                actual == &expected,
                "retained Tester log path differs from its command"
            );
            crate::verification_assessment::bound_recovery_artifact(
                root,
                &format!("{area}/{expected}"),
            )?;
        }
    }
    if let Some(intent) = candidate.merge_state.landing_intent.as_ref() {
        anyhow::ensure!(
            intent.verified_commit == merged_commit
                && intent.check_artifacts == candidate.merge_state.check_artifacts,
            "retained landing intent differs from merged evidence"
        );
        for artifact in intent
            .check_artifacts
            .iter()
            .chain(&intent.review_artifacts)
        {
            crate::verification_assessment::validate_managed_file_path(root, &artifact.path)?;
        }
        anyhow::ensure!(
            landing_evidence_digest(root, &intent.check_artifacts, &intent.review_artifacts)?
                == intent.evidence_digest,
            "retained landing evidence changed after acceptance"
        );
    }
    Ok(path)
}

const MAX_EVIDENCE_BYTES: usize = 1024 * 1024;
const MAX_NATIVE_OBSERVATIONS: usize = 10_000;
pub(crate) fn candidate_workspace_path(
    project_root: &Path,
    output: &work_model::TaskOutput,
) -> PathBuf {
    let workspace = Path::new(&output.workspace_path);
    if workspace.is_absolute() {
        workspace.to_path_buf()
    } else {
        project_root.join(workspace)
    }
}

/// Prefer retained project objects; otherwise require the managed candidate
/// repository to contain every exact commit needed by the assessment.
pub(crate) fn committed_object_repository(
    project_root: &Path,
    workspace_path: &str,
    commits: &[&str],
) -> Result<PathBuf> {
    anyhow::ensure!(
        !commits.is_empty()
            && commits.iter().all(|commit| {
                commit.len() == 40
                    && commit
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            }),
        "committed object lookup requires full lowercase commit SHAs"
    );
    let contains_commits = |repository: &Path| {
        commits.iter().all(|commit| {
            crate::git::run_raw(
                repository,
                &["cat-file", "-e", &format!("{commit}^{{commit}}")],
            )
            .is_ok_and(|output| output.status.success())
        })
    };
    if contains_commits(project_root) {
        return Ok(project_root.to_path_buf());
    }
    let workspace = work_model::resolve_managed_sibling_workspace_path(
        project_root,
        workspace_path,
        "committed object workspace",
    )?;
    anyhow::ensure!(
        contains_commits(&workspace),
        "exact committed objects are unavailable"
    );
    Ok(workspace)
}

pub(crate) fn validate_managed_file_path(project_root: &Path, relative: &str) -> Result<PathBuf> {
    let relative_path = Path::new(relative);
    let managed_root = Path::new(".fluent/work/artifacts");
    if relative_path.components().collect::<PathBuf>().as_os_str() != relative_path.as_os_str()
        || relative_path.as_os_str().is_empty()
        || relative_path.is_absolute()
        || !relative_path.starts_with(managed_root)
    {
        bail!("artifact path must stay under the managed artifact root");
    }
    let components = relative_path.components().collect::<Vec<_>>();
    if components.len() <= managed_root.components().count()
        || components
            .iter()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("artifact path must use normal managed components");
    }
    let full_path = project_root.join(relative_path);
    let mut current = project_root.to_path_buf();
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(part) = component else {
            unreachable!("validated artifact component");
        };
        current.push(part);
        let metadata = fs::symlink_metadata(&current)?;
        if metadata.file_type().is_symlink() {
            bail!("artifact path must not traverse a symlink");
        }
        let is_last = index + 1 == components.len();
        if (is_last && !metadata.is_file()) || (!is_last && !metadata.is_dir()) {
            bail!("artifact path has an invalid file type");
        }
    }
    Ok(full_path)
}

pub(crate) fn parse_evidence_document(bytes: &[u8]) -> Result<EvidenceDocument> {
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
    if document.native_observations.len() > MAX_NATIVE_OBSERVATIONS {
        bail!("evidence document has too many native observations (max {MAX_NATIVE_OBSERVATIONS})");
    }
    let mut seen_ids = std::collections::BTreeSet::new();
    for obs in &document.native_observations {
        if obs.id.trim().is_empty() || obs.harness.trim().is_empty() {
            bail!("native observation must have non-empty id and harness");
        }
        if !seen_ids.insert(obs.id.as_str()) {
            bail!("native observations must have unique ids");
        }
    }
    Ok(document)
}

pub(crate) fn review_artifact_path(task: &Task) -> Option<String> {
    task.artifact_area
        .as_ref()
        .map(|area| format!("{}/review.md", area.path))
}
/// Evaluate canonical checks without publishing state or scheduling work.
pub(crate) struct CanonicalCheckAssessment {
    pub config_digest: String,
    pub results_path: PathBuf,
    pub checks: Vec<work_model::RequiredCheck>,
    pub outstanding: Vec<usize>,
    pub requested: Vec<UnresolvedReviewCheck>,
    pub accepted: Vec<usize>,
    pub preexisting: Vec<usize>,
    pub baseline: Option<work_model::BoundRecheckArtifact>,
    pub review_bindings: std::collections::HashMap<usize, Vec<work_model::BoundRecheckArtifact>>,
    pub blockers: Vec<String>,
    /// Distinguish broken evidence bindings from renewable reviewer checks.
    pub has_integrity_blockers: bool,
    pub diagnoses: Vec<String>,
    /// Demonstrated assertion or harness defects retain source correction.
    pub defects: Vec<CanonicalDefect>,
    pub reconstruction: Option<CanonicalReconstruction>,
}

pub(crate) struct CanonicalDefect {
    pub id: String,
    pub diagnostic: String,
}

pub(crate) struct CanonicalReconstruction {
    pub manifest: work_model::BoundRecheckArtifact,
    pub manifest_bytes: Vec<u8>,
    pub audit: work_model::BoundRecheckArtifact,
    pub audit_bytes: Vec<u8>,
}

impl CanonicalCheckAssessment {
    pub fn is_satisfied(&self) -> bool {
        self.outstanding.is_empty() && self.blockers.is_empty() && self.defects.is_empty()
    }
}

pub(crate) fn bound_recovery_artifact(
    project_root: &Path,
    path: &str,
) -> Result<(work_model::BoundRecheckArtifact, Vec<u8>)> {
    let resolved = validate_managed_file_path(project_root, path)?;
    anyhow::ensure!(
        fs::metadata(&resolved)?.len() <= 16 * MAX_EVIDENCE_BYTES as u64,
        "verification artifact exceeds the bounded read limit: {path}"
    );
    let bytes = fs::read(resolved)?;
    Ok((
        work_model::BoundRecheckArtifact {
            path: path.to_string(),
            digest: format!("sha256:{:x}", Sha256::digest(&bytes)),
        },
        bytes,
    ))
}

/// Select by candidate and configured command position; Task identities only
/// locate the immutable execution that supplies the complete-run evidence.
pub(crate) fn assess_canonical_checks(
    project_root: &Path,
    attempt: &work_model::Attempt,
) -> Result<CanonicalCheckAssessment> {
    assess_canonical_checks_at(
        project_root,
        attempt,
        crate::check_applicability::evaluation_time(),
    )
}

fn assess_canonical_checks_at(
    project_root: &Path,
    attempt: &work_model::Attempt,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<CanonicalCheckAssessment> {
    let (candidate_commit, workspace_path) = if let Some(output) = attempt
        .tasks
        .iter()
        .rev()
        .find(|task| task.kind == TaskKind::Write && task.status == TaskStatus::Complete)
        .and_then(|task| task.output.as_ref())
    {
        (output.commit.as_str(), output.workspace_path.as_str())
    } else {
        anyhow::ensure!(
            attempt.kind.is_review_only_like(),
            "no completed Writer candidate"
        );
        // Review-only Attempts start with a Tester bound to the reviewed commit.
        let context = attempt
            .tasks
            .first()
            .filter(|task| task.kind == TaskKind::Tester)
            .and_then(|task| task.review_context.as_ref())
            .context("review-only Tester comparison context is missing")?;
        (
            context.candidate_commit.as_str(),
            context.candidate_workspace_path.as_str(),
        )
    };
    let workspace = project_root.join(workspace_path);
    let tester = attempt
        .tasks
        .iter()
        .rev()
        .find(|task| {
            task.kind == TaskKind::Tester
                && task.status == TaskStatus::Complete
                && !task.is_failed_command_recheck()
                && task
                    .review_context
                    .as_ref()
                    .is_some_and(|context| context.candidate_commit == candidate_commit)
        })
        .context("no complete canonical Tester for the current candidate")?;
    let context = tester
        .review_context
        .as_ref()
        .context("Tester comparison context is missing")?;
    let area = &tester
        .artifact_area
        .as_ref()
        .context("Tester artifact area is missing")?
        .path;
    anyhow::ensure!(
        *area == work_model::work_artifact_path(&attempt.work_item_id, &attempt.id, &tester.id),
        "Tester artifact area is not canonical"
    );
    let (results_ref, result_bytes) =
        bound_recovery_artifact(project_root, &format!("{area}/tester-results.json"))?;
    let results: crate::tester::TesterResults = serde_json::from_slice(&result_bytes)?;
    anyhow::ensure!(
        results.candidate_commit.as_deref() == Some(candidate_commit),
        "canonical Tester result has a different or missing candidate binding"
    );
    // The pre-write baseline explains suite health; it never supplies a
    // passing result for an explicit reference. Keep the exact read as proof.
    let baseline_path = format!(
        "{}/tester-results.json",
        work_model::work_artifact_path(
            &attempt.work_item_id,
            &attempt.id,
            &format!("{}-baseline-tester", attempt.id)
        )
    );
    let baseline = match fs::symlink_metadata(project_root.join(&baseline_path)) {
        Ok(_) => {
            let (binding, bytes) = bound_recovery_artifact(project_root, &baseline_path)?;
            serde_json::from_slice::<crate::tester::TesterResults>(&bytes)
                .ok()
                .map(|results| (binding, results))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let baseline_results = baseline.as_ref().map(|(_, results)| results);
    let preexisting_commands =
        crate::tester_evidence::preexisting_failed_tester_commands(&results, baseline_results);
    let mut preexisting = Vec::new();
    let object_repository =
        committed_object_repository(project_root, workspace_path, &[candidate_commit])?;
    let committed_bytes = |path: &str| -> Result<Vec<u8>> {
        let committed = crate::git::run_raw(
            &object_repository,
            &["show", &format!("{}:{path}", candidate_commit)],
        )?;
        anyhow::ensure!(
            committed.status.success(),
            "cannot read committed checker configuration: {path}"
        );
        Ok(committed.stdout)
    };
    let config_bytes = committed_bytes(".fluent/tester.yaml")?;
    let extractor_bytes = committed_bytes(".fluent/extract-tester-results")?;
    let original_manifest_path = format!("{area}/canonical-run.json");
    let mut reconstruction = None;
    let (manifest_ref, manifest_bytes) = match fs::symlink_metadata(
        project_root.join(&original_manifest_path),
    ) {
        Ok(_) => bound_recovery_artifact(project_root, &original_manifest_path)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // Support one older shape: an exact candidate-bound completed full
            // Tester whose complete ordered commands prove default code checks.
            let manifest = reconstruct_legacy_manifest(&config_bytes, &extractor_bytes, &results)?;
            let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
            let directory = work_model::work_artifact_path(
                &attempt.work_item_id,
                &attempt.id,
                &format!("evidence-reconstructions/{}", tester.id),
            );
            let manifest_ref = work_model::BoundRecheckArtifact {
                path: format!("{directory}/canonical-run.json"),
                digest: format!("sha256:{:x}", Sha256::digest(&manifest_bytes)),
            };
            let audit_bytes = serde_json::to_vec_pretty(&serde_json::json!({
                "schema_version": 1, "work_item_id": attempt.work_item_id, "attempt_id": attempt.id,
                "candidate_commit": candidate_commit, "source_task_id": tester.id,
                "source_task_kind": tester.kind, "source_task_role": tester.role,
                "source_task_context": context, "source_results": results_ref,
                "configuration": {"git_object":format!("{}:.fluent/tester.yaml", candidate_commit), "digest":manifest.config_digest},
                "extractor": {"git_object":format!("{}:.fluent/extract-tester-results", candidate_commit), "digest":manifest.extractor_digest},
                "derived_manifest": manifest_ref,
                "basis": "Completed canonical Tester and exact ordered command coverage; default candidate-code applicability only"
            }))?;
            anyhow::ensure!(
                audit_bytes.len() <= MAX_EVIDENCE_BYTES,
                "legacy reconstruction audit exceeds its bound"
            );
            let audit = work_model::BoundRecheckArtifact {
                path: format!("{directory}/audit.json"),
                digest: format!("sha256:{:x}", Sha256::digest(&audit_bytes)),
            };
            for (artifact, expected) in [(&manifest_ref, &manifest_bytes), (&audit, &audit_bytes)] {
                match fs::symlink_metadata(project_root.join(&artifact.path)) {
                    Ok(_) => anyhow::ensure!(
                        bound_recovery_artifact(project_root, &artifact.path)?.1 == *expected,
                        "preserved legacy reconstruction bytes changed: {}",
                        artifact.path
                    ),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
            reconstruction = Some(CanonicalReconstruction {
                manifest: manifest_ref.clone(),
                manifest_bytes: manifest_bytes.clone(),
                audit,
                audit_bytes,
            });
            (manifest_ref, manifest_bytes)
        }
        Err(error) => return Err(error.into()),
    };
    let manifest: crate::tester::CanonicalRunManifest = serde_json::from_slice(&manifest_bytes)?;
    anyhow::ensure!(
        manifest.schema_version == 1 && manifest.full_run,
        "Tester manifest does not prove a complete canonical run"
    );
    for (path, bytes, expected) in [
        (
            ".fluent/tester.yaml",
            &config_bytes,
            &manifest.config_digest,
        ),
        (
            ".fluent/extract-tester-results",
            &extractor_bytes,
            &manifest.extractor_digest,
        ),
    ] {
        anyhow::ensure!(
            &format!("sha256:{:x}", Sha256::digest(bytes)) == expected,
            "canonical check configuration changed: {path}"
        );
        if path == ".fluent/tester.yaml" {
            let config: crate::tester::TesterConfig = serde_yaml::from_slice(bytes)?;
            anyhow::ensure!(
                config.commands == manifest.commands,
                "canonical manifest differs from configured commands"
            );
        }
    }
    let mut blockers = Vec::new();
    if results.commands.len() != manifest.commands.len() {
        blockers.push("canonical Tester is missing configured command results".into());
    }
    let mut defects = Vec::new();
    if let Some(error) = &results.error {
        if tester_error_is_candidate_defect(error) {
            defects.push(CanonicalDefect {
                id: format!("tester-harness:{}:{}", manifest.config_digest, error.kind),
                diagnostic: format!(
                    "canonical Tester has an unresolved extractor or harness error: {}",
                    error.kind
                ),
            });
        } else {
            blockers.push(format!(
                "canonical Tester needs infrastructure diagnosis: {}",
                error.kind
            ));
        }
    }
    if results.summary.total != results.tests.len()
        || results.summary.pass
            != results
                .tests
                .iter()
                .filter(|test| test.status == "pass")
                .count()
        || results.summary.fail
            != results
                .tests
                .iter()
                .filter(|test| test.status == "fail")
                .count()
        || results.summary.skipped
            != results
                .tests
                .iter()
                .filter(|test| matches!(test.status.as_str(), "skipped" | "not_run"))
                .count()
    {
        blockers
            .push("canonical Tester summary does not account for its structured results".into());
    }
    let mut logs = Vec::new();
    for (index, command) in results.commands.iter().enumerate() {
        for (actual, expected) in [
            (&command.stdout_log, format!("commands/{index}-stdout.log")),
            (&command.stderr_log, format!("commands/{index}-stderr.log")),
        ] {
            anyhow::ensure!(
                actual == &expected,
                "canonical command log path does not match command {index}"
            );
            logs.push(bound_recovery_artifact(project_root, &format!("{area}/{expected}"))?.0);
        }
        if command
            .process_settlement
            .as_ref()
            .is_some_and(|settlement| !settlement.leaks.is_empty())
        {
            blockers.push(format!("command {index} has unresolved process leaks"));
        }
    }
    let requested = unresolved_review_checks(project_root, attempt, now);
    let reviewed = accepted_review_check_results(project_root, attempt, now);
    let rechecked = attempt
        .evidence_recoveries
        .iter()
        .filter_map(|recovery| accepted_recheck_result(project_root, attempt, recovery))
        .collect::<Vec<_>>();
    let mut checks = Vec::new();
    let mut outstanding = Vec::new();
    let mut accepted = Vec::new();
    let mut review_bindings = std::collections::HashMap::new();
    let mut diagnoses = Vec::new();
    for (index, definition) in manifest.commands.iter().enumerate() {
        let applicability = match manifest.applicability.get(index) {
            Some(binding) => binding.clone(),
            None if definition.applicability.is_default() => {
                work_model::ResolvedCheckApplicability::default()
            }
            None => bail!("required command applicability snapshot is missing"),
        };
        anyhow::ensure!(
            applicability.definition == definition.applicability
                && resolve_check_applicability(&workspace, &definition.applicability)?
                    == applicability,
            "declared comparison ref or check applicability changed since execution"
        );
        let check = work_model::RequiredCheck {
            applicability,
            id: format!(
                "tester:{}:{index}",
                manifest
                    .config_digest
                    .strip_prefix("sha256:")
                    .context("invalid Tester configuration digest")?
            ),
            work_item_id: attempt.work_item_id.clone(),
            attempt_id: attempt.id.clone(),
            candidate_commit: candidate_commit.to_string(),
            command: definition.command.clone(),
            working_directory: ".".into(),
            source: work_model::RequiredCheckSource::CanonicalTester {
                reconstruction: reconstruction.as_ref().map(|proof| proof.audit.clone()),
                task_id: tester.id.clone(),
                command_index: index,
                results: results_ref.clone(),
                manifest: manifest_ref.clone(),
                logs: logs.clone(),
                comparison_commit: context.base_commit.clone(),
            },
        };
        let result = results.commands.get(index);
        let matches = result.is_some_and(|command| {
            command.command == definition.command && command.test_harness == definition.test_harness
        });
        if !matches {
            blockers.push(format!(
                "command {index} does not match the canonical manifest"
            ));
        }
        let unmet_prerequisite = result.and_then(|command| match &command.failure_cause {
            Some(crate::tester::CommandFailureCause::UnmetPrerequisite { fact, required, observed })
                if matches && command.exit_code != 0 && command.duration_ms == 0
                    && matches!(fact.as_str(), "os" | "architecture")
                    && definition.applicability.environment.get(fact) == Some(required)
                    && manifest.environment.get(index).and_then(|facts| facts.get(fact)) == Some(observed)
                    && observed != required => Some(format!("{fact}: required {required}, observed {observed}; prerequisite prevented execution")),
            _ => None,
        });
        if definition.reject_process_leaks
            && unmet_prerequisite.is_none()
            && result.is_none_or(|command| command.process_settlement.is_none())
        {
            blockers.push(format!(
                "command {index} has no required process-settlement proof"
            ));
        }
        let local_evidence = EvidenceDocument {
            schema_version: 1,
            producer: tester.id.clone(),
            check: check.command.clone(),
            working_directory: check.working_directory.clone(),
            result: EvidenceResult::Pass,
            output: String::new(),
            run_at: manifest.run_at.clone().unwrap_or_default(),
            environment: manifest.environment.get(index).cloned().unwrap_or_default(),
            comparison_commits: check.applicability.comparison_commits.clone(),
            native_observations: vec![],
        };
        let local_applies =
            validate_check_applicability_at(&check.applicability, &local_evidence, now).is_ok();
        if !matches || !local_applies || result.is_none_or(|command| command.exit_code != 0) {
            let unique_command = manifest
                .commands
                .iter()
                .filter(|other| other.command == definition.command)
                .count()
                == 1;
            let reviewed_replacement = reviewed.iter().find(|result| {
                unique_command
                    && result.command == check.command
                    && result.applicability == check.applicability
                    && result.comparison_commit == context.base_commit
            });
            let recheck_replacement = rechecked.iter().find(|proof| {
                proof.source_results == results_ref
                    && proof.command_index == index
                    && proof.command == check.command
                    && proof.comparison_commit == context.base_commit
                    && check.applicability == work_model::ResolvedCheckApplicability::default()
            });
            if matches
                && (validate_required_check_acceptance_at(project_root, attempt, &check, now)
                    .is_ok()
                    || reviewed_replacement.is_some()
                    || recheck_replacement.is_some())
            {
                accepted.push(index);
                if let Some(proof) = reviewed_replacement {
                    review_bindings.insert(index, proof.bindings.clone());
                } else if let Some(proof) = recheck_replacement {
                    review_bindings.insert(index, proof.bindings.clone());
                }
            } else if matches && local_applies && preexisting_commands.contains(&index) {
                preexisting.push(index);
            } else {
                outstanding.push(index);
                if let Some(reason) = unmet_prerequisite {
                    diagnoses.push(format!("Required check {}: {reason}. Repair the declared environment or submit authorized host proof.", check.id));
                } else if results.error.is_none()
                    && !results.tests.iter().any(|test| test.status == "fail")
                    && result.is_some_and(|command| command.exit_code != 0)
                {
                    diagnoses.push(format!("Required check {} needs diagnosis: a nonzero exit does not establish an environment, source, or harness cause. Preserve the logs and establish the missing cause or submit exact authorized host proof.", check.id));
                }
            }
        }
        checks.push(check);
    }
    let command_identities = manifest
        .commands
        .iter()
        .map(|command| (command.command.as_str(), command.test_harness.as_str()))
        .collect::<Vec<_>>();
    for test in results.tests.iter().filter(|test| test.status == "fail") {
        let covered = failed_test_covered_by_accepted_command(&command_identities, &accepted, test);
        let preexisting = baseline_results.is_some_and(|baseline| {
            baseline
                .tests
                .iter()
                .any(|prior| prior.status == "fail" && prior.id == test.id)
        });
        if !covered && !preexisting {
            let native = references::normalized_result(test)
                .map(|test| test.id)
                .unwrap_or_else(|_| test.id.clone());
            defects.push(CanonicalDefect {
                id: format!(
                    "tester-failure:{}:{}",
                    manifest.config_digest,
                    crate::verification_policy::digest(native.as_bytes())
                ),
                diagnostic: format!(
                    "canonical Tester has an unresolved structured failure: {}",
                    test.id
                ),
            });
        }
    }
    defects.sort_by(|a, b| a.id.cmp(&b.id));
    defects.dedup_by(|a, b| a.id == b.id);
    let has_integrity_blockers = !blockers.is_empty();
    blockers.extend(requested.iter().map(|check| check.reason.clone()));
    Ok(CanonicalCheckAssessment {
        has_integrity_blockers,
        config_digest: manifest.config_digest,
        requested,
        defects,
        reconstruction,
        diagnoses,
        results_path: project_root.join(results_ref.path),
        checks,
        outstanding,
        accepted,
        preexisting,
        baseline: baseline.map(|(binding, _)| binding),
        review_bindings,
        blockers,
    })
}

pub(crate) fn failed_test_covered_by_accepted_command(
    commands: &[(&str, &str)],
    accepted: &[usize],
    test: &crate::tester::TestResult,
) -> bool {
    let matching = commands
        .iter()
        .enumerate()
        .filter(|(_, (_, harness))| *harness == test.test_harness)
        .collect::<Vec<_>>();
    // The extractor records a native test's harness but not its command index.
    // Attribute it to a reviewed replacement only when that harness occurs in
    // exactly one configured command. A synthetic command failure has its own
    // exact command identity, even when a harness is configured more than once.
    matching.iter().any(|(index, (command, _))| {
        accepted.contains(index)
            && (test.id == format!("command-{index}:{command}") || matching.len() == 1)
    })
}

#[cfg(test)]
mod command_failure_attribution_tests {
    use super::failed_test_covered_by_accepted_command;
    use crate::tester::TestResult;

    fn failure(id: &str, harness: &str) -> TestResult {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "test_harness": harness,
            "status": "fail",
            "duration_ms": 1,
            "failure_excerpt": "fixture socket was denied"
        }))
        .unwrap()
    }

    #[test]
    fn reviewed_unique_harness_command_covers_its_native_failures() {
        let commands = [
            ("cargo nextest run", "cargo-nextest"),
            ("bash checks.sh", "shell"),
        ];
        let native = failure(
            "fluent::dashboard_terminal$fullscreen_resize_requires_control",
            "cargo-nextest",
        );
        assert!(failed_test_covered_by_accepted_command(
            &commands,
            &[0],
            &native
        ));
        assert!(!failed_test_covered_by_accepted_command(
            &commands,
            &[1],
            &native
        ));
    }

    #[test]
    fn duplicate_harness_requires_exact_synthetic_command_identity() {
        let commands = [
            ("cargo nextest run -E one", "cargo-nextest"),
            ("cargo nextest run -E two", "cargo-nextest"),
        ];
        let native = failure(
            "fluent::dashboard_terminal$fullscreen_resize_requires_control",
            "cargo-nextest",
        );
        assert!(!failed_test_covered_by_accepted_command(
            &commands,
            &[0],
            &native
        ));
        let synthetic = failure("command-0:cargo nextest run -E one", "cargo-nextest");
        assert!(failed_test_covered_by_accepted_command(
            &commands,
            &[0],
            &synthetic
        ));
        assert!(!failed_test_covered_by_accepted_command(
            &commands,
            &[1],
            &synthetic
        ));
    }
}

pub(crate) fn validate_required_check_acceptance_at(
    project_root: &Path,
    attempt: &work_model::Attempt,
    check: &work_model::RequiredCheck,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    // A newer conflicting result must receive its own review; the older pass
    // cannot silently defeat the operator's subsequently submitted evidence.
    let recovery = attempt
        .evidence_recoveries
        .iter()
        .rev()
        .find(|recovery| {
            recovery.required_check.as_ref().is_some_and(|bound| {
                bound.id == check.id && bound.candidate_commit == check.candidate_commit
            })
        })
        .context("required check has no recorded evidence")?;
    anyhow::ensure!(
        recovery.required_check.as_ref() == Some(check)
            && recovery.state == EvidenceRecoveryState::Complete
            && recovery.targets.len() == 1,
        "required check needs completed targeted evidence"
    );
    let (attachment, bytes) =
        bound_recovery_artifact(project_root, &recovery.attachment.snapshot_path)?;
    anyhow::ensure!(
        attachment.digest == recovery.attachment.digest,
        "host evidence bytes changed"
    );
    let document = parse_evidence_document(&bytes)?;
    validate_check_applicability_at(&check.applicability, &document, now)?;
    anyhow::ensure!(
        document.result == EvidenceResult::Pass
            && document.check == check.command
            && document.working_directory == check.working_directory,
        "host evidence does not pass this exact check"
    );
    let target = &recovery.targets[0];
    let task = attempt
        .tasks
        .iter()
        .find(|task| Some(task.id.as_str()) == target.review_task_id.as_deref())
        .context("missing targeted review")?;
    let context = task
        .evidence_review_context
        .as_ref()
        .context("missing evidence review context")?;
    anyhow::ensure!(
        task.kind == TaskKind::Review
            && task.role == "tests"
            && task.status == TaskStatus::Complete
            && context.recovery_id == recovery.id
            && context.candidate_commit == check.candidate_commit
            && context.attachment == recovery.attachment
            && context.prior_review_artifact == target.prior_review_artifact,
        "targeted review binding changed"
    );
    let (_, binding) = bound_recovery_artifact(project_root, &target.prior_review_artifact)?;
    match &check.source {
        work_model::RequiredCheckSource::CanonicalTester {
            results,
            manifest,
            logs,
            reconstruction,
            ..
        } => {
            anyhow::ensure!(
                serde_json::from_slice::<work_model::RequiredCheck>(&binding)? == *check,
                "required check snapshot changed"
            );
            for source in std::iter::once(results)
                .chain(std::iter::once(manifest))
                .chain(logs)
                .chain(reconstruction)
            {
                anyhow::ensure!(
                    bound_recovery_artifact(project_root, &source.path)?
                        .0
                        .digest
                        == source.digest,
                    "required source proof is missing or changed: {}",
                    source.path
                );
            }
        }
        work_model::RequiredCheckSource::ReviewRequested { request, .. } => anyhow::ensure!(
            request.path == target.prior_review_artifact
                && request.digest == format!("sha256:{:x}", Sha256::digest(&binding)),
            "review request snapshot changed"
        ),
    }
    let path = format!(
        "{}/review.md",
        task.artifact_area
            .as_ref()
            .context("missing review artifacts")?
            .path
    );
    let (artifact, bytes) = bound_recovery_artifact(project_root, &path)?;
    anyhow::ensure!(
        target.review_digest.as_deref() == Some(artifact.digest.as_str())
            && review::extract_verdict(std::str::from_utf8(&bytes)?) == review::Verdict::Pass,
        "targeted review has no accepted passing result"
    );
    Ok(())
}

pub(crate) fn validate_check_applicability_at(
    binding: &work_model::ResolvedCheckApplicability,
    document: &EvidenceDocument,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    for (name, expected) in &binding.definition.environment {
        anyhow::ensure!(
            document.environment.get(name) == Some(expected),
            "required environment fact {name} is missing or changed"
        );
    }
    anyhow::ensure!(
        binding
            .comparison_commits
            .iter()
            .all(|(name, commit)| document.comparison_commits.get(name) == Some(commit)),
        "required comparison commit is missing or changed"
    );
    if let work_model::CheckFreshness::LiveState { max_age_seconds } = binding.definition.freshness
    {
        let seconds = max_age_seconds
            .filter(|seconds| *seconds > 0 && *seconds <= i64::MAX as u64)
            .context("live-state evidence is missing a supported freshness expiry")?;
        let recorded = DateTime::parse_from_rfc3339(&document.run_at)?.with_timezone(&chrono::Utc);
        let expiry = chrono::TimeDelta::try_seconds(seconds as i64)
            .and_then(|duration| recorded.checked_add_signed(duration))
            .context("unsupported live-state freshness expiry")?;
        anyhow::ensure!(
            recorded <= now && now < expiry,
            "live-state evidence has expired or has an unverified future timestamp"
        );
    }
    Ok(())
}

pub(crate) fn validate_check_applicability(
    workspace: &Path,
    binding: &work_model::ResolvedCheckApplicability,
    document: &EvidenceDocument,
) -> Result<()> {
    validate_check_applicability_in_workspace_at(
        workspace,
        binding,
        document,
        crate::check_applicability::evaluation_time(),
    )
}

pub(crate) fn validate_check_applicability_in_workspace_at(
    workspace: &Path,
    binding: &work_model::ResolvedCheckApplicability,
    document: &EvidenceDocument,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    anyhow::ensure!(
        resolve_check_applicability(workspace, &binding.definition)? == *binding,
        "declared comparison ref changed since execution"
    );
    validate_check_applicability_at(binding, document, now)
}

pub(crate) fn validate_required_check_binding(
    project_root: &Path,
    attempt: &work_model::Attempt,
    check: &work_model::RequiredCheck,
) -> Result<()> {
    let output = attempt
        .tasks
        .iter()
        .rev()
        .find(|task| task.kind == TaskKind::Write && task.status == TaskStatus::Complete)
        .and_then(|task| task.output.as_ref())
        .context("required check has no current candidate")?;
    anyhow::ensure!(
        check.work_item_id == attempt.work_item_id
            && check.attempt_id == attempt.id
            && check.candidate_commit == output.commit,
        "required check belongs to another candidate or Attempt"
    );
    match &check.source {
        work_model::RequiredCheckSource::CanonicalTester {
            results,
            manifest,
            logs,
            reconstruction,
            ..
        } => {
            for source in std::iter::once(results)
                .chain(std::iter::once(manifest))
                .chain(logs)
                .chain(reconstruction)
            {
                anyhow::ensure!(
                    bound_recovery_artifact(project_root, &source.path)?.0 == *source,
                    "required source digest changed: {}",
                    source.path
                );
            }
            anyhow::ensure!(
                assess_canonical_checks(project_root, attempt)?
                    .checks
                    .contains(check),
                "required canonical check changed"
            );
        }
        work_model::RequiredCheckSource::ReviewRequested {
            task_id,
            request,
            comparison_commit,
        } => {
            let task = attempt
                .tasks
                .iter()
                .find(|task| {
                    task.id == *task_id
                        && task.kind == TaskKind::Review
                        && task.status == TaskStatus::Complete
                })
                .context("required reviewer request has no completed source Task")?;
            let context = task
                .review_context
                .as_ref()
                .context("reviewer request has no comparison context")?;
            anyhow::ensure!(
                context.candidate_commit == check.candidate_commit
                    && &context.base_commit == comparison_commit
                    && task
                        .artifact_area
                        .as_ref()
                        .is_some_and(|area| request.path == format!("{}/review.md", area.path)),
                "review request source binding changed"
            );
            let (actual, bytes) = bound_recovery_artifact(project_root, &request.path)?;
            anyhow::ensure!(actual == *request, "review request source bytes changed");
            let definition = review::extract_focused_evidence_request(std::str::from_utf8(&bytes)?)
                .context("review request is missing its command declaration")?;
            anyhow::ensure!(
                definition.command == check.command
                    && definition.working_directory == check.working_directory
                    && resolve_check_applicability(
                        &candidate_workspace_path(project_root, output),
                        &definition.applicability
                    )? == check.applicability,
                "review request command, environment, freshness, or comparison changed"
            );
        }
    }
    Ok(())
}

/// Reconstruct only inputs proved by the completed producer and committed code.
pub(crate) fn reconstruct_legacy_manifest(
    config_bytes: &[u8],
    extractor_bytes: &[u8],
    results: &crate::tester::TesterResults,
) -> Result<crate::tester::CanonicalRunManifest> {
    anyhow::ensure!(
        config_bytes.len() <= MAX_EVIDENCE_BYTES && extractor_bytes.len() <= 4 * MAX_EVIDENCE_BYTES,
        "legacy checker source exceeds the supported bound"
    );
    let config: crate::tester::TesterConfig = serde_yaml::from_slice(config_bytes)?;
    anyhow::ensure!(
        !config.commands.is_empty()
            && config.commands.len() <= 1024
            && config.commands.len() == results.commands.len(),
        "legacy Tester lacks complete ordered command coverage"
    );
    for (definition, result) in config.commands.iter().zip(&results.commands) {
        anyhow::ensure!(
            definition.applicability.is_default(),
            "legacy Tester lacks historical applicability proof; current environment facts or comparison refs cannot establish an older run's inputs"
        );
        anyhow::ensure!(
            definition.command == result.command && definition.test_harness == result.test_harness,
            "legacy Tester command or harness differs from its committed configuration"
        );
    }
    Ok(crate::tester::CanonicalRunManifest {
        schema_version: 1,
        config_digest: format!("sha256:{:x}", Sha256::digest(config_bytes)),
        extractor_digest: format!("sha256:{:x}", Sha256::digest(extractor_bytes)),
        commands: config.commands,
        full_run: true,
        run_at: None,
        environment: vec![],
        applicability: vec![],
    })
}

pub(crate) struct UnresolvedReviewCheck {
    pub check: work_model::RequiredCheck,
    pub reason: String,
    pub can_refresh: bool,
}

fn unresolved_review_checks(
    project_root: &Path,
    attempt: &work_model::Attempt,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<UnresolvedReviewCheck> {
    let candidate = attempt
        .tasks
        .iter()
        .rev()
        .find(|task| task.kind == TaskKind::Write && task.status == TaskStatus::Complete)
        .and_then(|task| task.output.as_ref())
        .map(|output| output.commit.as_str());
    let mut seen = std::collections::HashSet::new();
    let mut unresolved = Vec::new();
    for check in attempt
        .evidence_recoveries
        .iter()
        .rev()
        .filter_map(|recovery| recovery.required_check.as_ref())
    {
        let work_model::RequiredCheckSource::ReviewRequested { request, .. } = &check.source else {
            continue;
        };
        if Some(check.candidate_commit.as_str()) != candidate || !seen.insert(check.id.clone()) {
            continue;
        }
        let accepted =
            validate_required_check_binding(project_root, attempt, check).and_then(|()| {
                validate_required_check_acceptance_at(project_root, attempt, check, now)
            });
        if let Err(error) = accepted {
            let can_refresh =
                expired_completed_review_recovery_at(project_root, attempt, &request.path, now)
                    .is_ok_and(|recovery| recovery.is_some());
            unresolved.push(UnresolvedReviewCheck {
                check: check.clone(),
                reason: format!("Required reviewer check {} remains unresolved: `{}` (working directory {}, source {}): {error:#}", check.id, check.command, check.working_directory, request.path),
                can_refresh,
            });
        }
    }
    unresolved
}

/// Authorize renewing an expired completed request only while every other
/// recorded binding, including its passing Review, remains intact.
pub(crate) fn expired_completed_review_recovery<'a>(
    project_root: &Path,
    attempt: &'a work_model::Attempt,
    review_artifact: &str,
) -> Result<Option<&'a EvidenceRecovery>> {
    expired_completed_review_recovery_at(
        project_root,
        attempt,
        review_artifact,
        crate::check_applicability::evaluation_time(),
    )
}

fn expired_completed_review_recovery_at<'a>(
    project_root: &Path,
    attempt: &'a work_model::Attempt,
    review_artifact: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Option<&'a EvidenceRecovery>> {
    let Some(recovery) = attempt.evidence_recoveries.iter().rev().find(|recovery| {
        recovery.required_check.as_ref().is_some_and(|check| matches!(&check.source,
            work_model::RequiredCheckSource::ReviewRequested { request, .. } if request.path == review_artifact))
    }) else { return Ok(None); };
    if recovery.state != EvidenceRecoveryState::Complete {
        return Ok(None);
    }
    let check = recovery.required_check.as_ref().unwrap();
    let latest_review = attempt
        .tasks
        .iter()
        .rev()
        .find(|task| task.kind == TaskKind::Review && task.role == "tests")
        .context("required reviewer check has no latest Tests Review")?;
    let path =
        review_artifact_path(latest_review).context("latest Tests Review has no artifact")?;
    let (_, review_bytes) = bound_recovery_artifact(project_root, &path)?;
    anyhow::ensure!(
        latest_review.status == TaskStatus::Complete
            && latest_review
                .review_context
                .as_ref()
                .is_some_and(|context| context.candidate_commit == check.candidate_commit)
            && review::extract_verdict(std::str::from_utf8(&review_bytes)?)
                == review::Verdict::Pass,
        "the latest Tests Review must pass before renewing completed evidence"
    );

    validate_required_check_binding(project_root, attempt, check)?;
    let work_model::CheckFreshness::LiveState {
        max_age_seconds: Some(seconds),
    } = check.applicability.definition.freshness
    else {
        return Ok(None);
    };
    let (_, bytes) = bound_recovery_artifact(project_root, &recovery.attachment.snapshot_path)?;
    let document = parse_evidence_document(&bytes)?;
    let issued = DateTime::parse_from_rfc3339(&document.run_at)?.with_timezone(&chrono::Utc);
    let expiry = chrono::TimeDelta::try_seconds(i64::try_from(seconds)?)
        .and_then(|duration| issued.checked_add_signed(duration))
        .context("unsupported live-state freshness expiry")?;
    if now < expiry {
        return Ok(None);
    }
    validate_required_check_acceptance_at(project_root, attempt, check, issued)?;
    Ok(Some(recovery))
}

struct ReviewedCheckResult {
    command: String,
    applicability: work_model::ResolvedCheckApplicability,
    comparison_commit: Option<String>,
    bindings: Vec<work_model::BoundRecheckArtifact>,
}

pub(crate) fn completed_review_check_commands(
    project_root: &Path,
    attempt: &work_model::Attempt,
) -> Vec<String> {
    let mut commands = accepted_review_check_results(
        project_root,
        attempt,
        crate::check_applicability::evaluation_time(),
    )
    .into_iter()
    .map(|result| result.command)
    .collect::<Vec<_>>();
    commands.sort();
    commands.dedup();
    commands
}

fn accepted_review_check_results(
    project_root: &Path,
    attempt: &work_model::Attempt,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<ReviewedCheckResult> {
    let latest_candidate = attempt
        .tasks
        .iter()
        .rev()
        .find(|task| task.kind == TaskKind::Write && task.status == TaskStatus::Complete)
        .and_then(|task| task.output.as_ref())
        .map(|output| output.commit.as_str());
    let Some(latest_candidate) = latest_candidate else {
        return Vec::new();
    };
    let read_verdict = |root: &Path, task: &Task| {
        let path = review_artifact_path(task)?;
        let (_, bytes) = bound_recovery_artifact(root, &path).ok()?;
        Some(review::extract_verdict(std::str::from_utf8(&bytes).ok()?))
    };
    let commands = attempt
        .evidence_recoveries
        .iter()
        .filter_map(|recovery| {
            if recovery.state != EvidenceRecoveryState::Complete
                || recovery.candidate_commit != latest_candidate
                || recovery.carried_finding.is_some()
            {
                return None;
            }
            if let Some(check) = &recovery.required_check {
                if !matches!(
                    check.source,
                    work_model::RequiredCheckSource::ReviewRequested { .. }
                ) || validate_required_check_binding(project_root, attempt, check).is_err()
                    || validate_required_check_acceptance_at(project_root, attempt, check, now)
                        .is_err()
                {
                    return None;
                }
            }
            let (_, evidence_bytes) =
                bound_recovery_artifact(project_root, &recovery.attachment.snapshot_path).ok()?;
            if format!("sha256:{:x}", Sha256::digest(&evidence_bytes)) != recovery.attachment.digest
            {
                return None;
            }
            let evidence = parse_evidence_document(&evidence_bytes).ok()?;
            if evidence.result != EvidenceResult::Pass {
                return None;
            }

            recovery.targets.iter().find_map(|target| {
                if target.role != "tests" {
                    return None;
                }
                let (_, prior_bytes) =
                    bound_recovery_artifact(project_root, &target.prior_review_artifact).ok()?;
                let prior_review = std::str::from_utf8(&prior_bytes).ok()?;
                let request = review::extract_focused_evidence_request(&prior_review)?;
                if recovery.required_check.is_none() && !request.applicability.is_default() {
                    return None;
                }
                if evidence.check != request.command
                    || request.working_directory != "."
                    || evidence.working_directory != request.working_directory
                {
                    return None;
                }
                let prior_task_is_bound = attempt.tasks.iter().any(|task| {
                    task.kind == TaskKind::Review
                        && task.role == "tests"
                        && task.status == TaskStatus::Complete
                        && task
                            .review_context
                            .as_ref()
                            .map(|context| context.candidate_commit.as_str())
                            == Some(latest_candidate)
                        && review_artifact_path(task).as_deref()
                            == Some(target.prior_review_artifact.as_str())
                        && read_verdict(project_root, task) == Some(review::Verdict::Fail)
                });
                let targeted_review_passed = target.review_task_id.as_ref().is_some_and(|id| {
                    attempt.tasks.iter().any(|task| {
                        task.id == *id
                            && task.kind == TaskKind::Review
                            && task.role == "tests"
                            && task.status == TaskStatus::Complete
                            && task
                                .evidence_review_context
                                .as_ref()
                                .is_some_and(|context| {
                                    context.recovery_id == recovery.id
                                        && context.candidate_commit == recovery.candidate_commit
                                        && context.attachment == recovery.attachment
                                        && context.prior_review_artifact
                                            == target.prior_review_artifact
                                })
                            && read_verdict(project_root, task) == Some(review::Verdict::Pass)
                    })
                });
                let source_task = attempt.tasks.iter().find(|task| {
                    review_artifact_path(task).as_deref()
                        == Some(target.prior_review_artifact.as_str())
                })?;
                (prior_task_is_bound && targeted_review_passed).then_some(ReviewedCheckResult {
                    command: request.command,
                    applicability: recovery
                        .required_check
                        .as_ref()
                        .map(|check| check.applicability.clone())
                        .unwrap_or_default(),
                    comparison_commit: source_task.review_context.as_ref()?.base_commit.clone(),
                    bindings: vec![
                        bound_recovery_artifact(project_root, &recovery.attachment.snapshot_path)
                            .ok()?
                            .0,
                        bound_recovery_artifact(project_root, &target.prior_review_artifact)
                            .ok()?
                            .0,
                        bound_recovery_artifact(
                            project_root,
                            &review_artifact_path(
                                attempt.tasks.iter().find(|task| {
                                    Some(&task.id) == target.review_task_id.as_ref()
                                })?,
                            )?,
                        )
                        .ok()?
                        .0,
                    ],
                })
            })
        })
        .collect::<Vec<_>>();
    commands
}

use crate::verification_reference::{self as references, ReferenceMeaning, TestReference};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VerificationCause {
    CandidateDefect,
    MissingEvidence,
    UninterpretableEvidence,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequirementAssessment {
    pub id: String,
    pub source: String,
    pub meaning: Option<ReferenceMeaning>,
    pub check: Option<work_model::RequiredCheck>,
    pub satisfied: bool,
    pub cause: Option<VerificationCause>,
    pub owner: work_model::CompletionResponsibility,
    pub next_action: Option<String>,
    pub bindings: Vec<work_model::BoundRecheckArtifact>,
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationAssessment {
    pub candidate_commit: String,
    pub references: Vec<RequirementAssessment>,
    pub commands: Vec<RequirementAssessment>,
}

impl VerificationAssessment {
    pub fn candidate_defects(&self) -> Vec<&RequirementAssessment> {
        self.references
            .iter()
            .chain(&self.commands)
            .filter(|row| !row.satisfied && row.cause == Some(VerificationCause::CandidateDefect))
            .collect()
    }

    pub fn unresolved(&self) -> impl Iterator<Item = &RequirementAssessment> {
        self.references
            .iter()
            .chain(&self.commands)
            .filter(|row| !row.satisfied)
    }
}

/// Evaluate reference occurrences and missing markers from immutable planning
/// text. Result prose explains the row without assigning its identity.
pub(crate) fn assess_references(
    workspace: &Path,
    work: &str,
    attempt: &str,
    candidate: &str,
    contract: &work_model::WriterCompletionContract,
    results: Option<&crate::tester::TesterResults>,
    canonical: Option<&CanonicalCheckAssessment>,
    bindings: &[work_model::BoundRecheckArtifact],
    accepted: &[AcceptedCommandProof],
) -> Result<Vec<RequirementAssessment>> {
    let mut rows = Vec::new();
    for requirement in contract
        .requirements
        .iter()
        .filter(|row| row.kind == work_model::WriterCompletionRequirementKind::Behavior)
    {
        if requirement.verification_refs.is_empty() {
            rows.push(RequirementAssessment {
                id: format!("reference:{work}:{attempt}:{}:missing", requirement.id),
                source: requirement.source.clone(),
                meaning: None,
                check: None,
                satisfied: false,
                cause: Some(VerificationCause::UninterpretableEvidence),
                owner: work_model::CompletionResponsibility::Tester,
                next_action: Some("Inspect the approved behavior and establish its intended Test: reference or Untestable: reason; clarification recording is not available in this version.".into()),
                bindings: bindings.to_vec(),
                diagnostics: vec![format!(
                    "{} has neither a Test: reference nor an Untestable: reason",
                    requirement.source
                )],
            });
            continue;
        }
        for (index, text) in requirement.verification_refs.iter().enumerate() {
            if text
                .strip_prefix("Untestable:")
                .is_some_and(|reason| !reason.trim().is_empty())
            {
                continue;
            }
            let mut row = RequirementAssessment {
                id: format!("reference:{work}:{attempt}:{}:{index}:{:x}", requirement.id, Sha256::digest(text.as_bytes())),
                source: requirement.source.clone(), check: None, meaning: None, satisfied: false,
                cause: Some(VerificationCause::UninterpretableEvidence),
                owner: work_model::CompletionResponsibility::Tester,
                next_action: Some("Inspect the approved reference and retained native evidence to establish its meaning; clarification recording is not available in this version.".into()),
                bindings: bindings.to_vec(), diagnostics: vec![],
            };
            let reference = match references::parse_test_reference(text) {
                Ok(reference) => reference,
                Err(reason) => {
                    row.diagnostics.push(format!(
                        "{} has malformed Test: reference {text:?}: {reason}",
                        requirement.source
                    ));
                    rows.push(row);
                    continue;
                }
            };
            row.meaning = Some(reference.meaning());
            if let Err(reason) = validate_reference_source(workspace, candidate, &reference) {
                row.cause = Some(VerificationCause::CandidateDefect);
                row.owner = requirement
                    .responsibility
                    .unwrap_or(work_model::CompletionResponsibility::Writer);
                row.next_action = Some(
                    "Repair the committed test or its approved verification reference.".into(),
                );
                row.diagnostics.push(reason.to_string());
                rows.push(row);
                continue;
            }
            let result =
                results.filter(|results| results.candidate_commit.as_deref() == Some(candidate));
            let declared_command_is_bound = if let Some(ReferenceMeaning::Command {
                command,
                command_index,
                ..
            }) = &reference.declared
            {
                let config = crate::git::run_raw(
                    workspace,
                    &["show", &format!("{candidate}:.fluent/tester.yaml")],
                )?;
                let config: Option<crate::tester::TesterConfig> = config
                    .status
                    .success()
                    .then(|| serde_yaml::from_slice(&config.stdout).ok())
                    .flatten();
                config.is_some_and(|config| {
                    let entries = config
                        .commands
                        .iter()
                        .enumerate()
                        .filter(|(index, entry)| {
                            entry.command == *command
                                && command_index.is_none_or(|expected| expected == *index)
                        })
                        .collect::<Vec<_>>();
                    let [(index, entry)] = entries.as_slice() else {
                        return false;
                    };
                    result.is_some_and(|results| {
                        results.commands.get(*index).is_some_and(|result| {
                            result.command == entry.command
                                && result.test_harness == entry.test_harness
                        })
                    })
                })
            } else {
                true
            };
            row.satisfied = declared_command_is_bound
                && result.is_some_and(|results| {
                    let library = match reference.meaning() {
                        ReferenceMeaning::CargoLibTest { name } => {
                            candidate_library_target(workspace, candidate, &name)
                        }
                        _ => None,
                    };
                    reference_is_satisfied(&reference, results, canonical, library.as_ref())
                });
            // A configured legacy command shares the canonical occurrence's
            // ambiguity, applicability, and comparison checks. The historical
            // adapter applies only after a current run proves the command lies
            // outside its configured checks.
            if reference.declared.is_none()
                && canonical.is_some_and(|view| {
                    !view
                        .checks
                        .iter()
                        .any(|check| Some(check.command.as_str()) == reference.command.as_deref())
                })
            {
                if let Some(proof) = accepted
                    .iter()
                    .find(|proof| reference.command.as_deref() == Some(proof.command.as_str()))
                {
                    row.satisfied = true;
                    row.owner = work_model::CompletionResponsibility::Host;
                    row.bindings.extend(proof.bindings.clone());
                }
            }
            if row.satisfied {
                row.cause = None;
                row.next_action = None;
            } else {
                row.cause = Some(VerificationCause::MissingEvidence);
                row.next_action = Some("Inspect the exact native identities and command logs, then collect candidate-bound evidence through the existing verification recovery path.".into());
                if let Some(results) = result {
                    for (index, command) in
                        results
                            .commands
                            .iter()
                            .enumerate()
                            .filter(|(index, command)| {
                                command.exit_code != 0
                                    && canonical.is_none_or(|view| !view.accepted.contains(index))
                            })
                    {
                        row.diagnostics.push(format!(
                            "Tester command {index} exited {}: {}",
                            command.exit_code, command.command
                        ));
                        if let Some(directory) = bindings
                            .first()
                            .and_then(|binding| Path::new(&binding.path).parent())
                        {
                            row.diagnostics.push(format!(
                                "Tester command {index} stdout log: {}",
                                directory.join(&command.stdout_log).display()
                            ));
                            row.diagnostics.push(format!(
                                "Tester command {index} stderr log: {}",
                                directory.join(&command.stderr_log).display()
                            ));
                        }
                    }
                }
                row.diagnostics
                    .push(format!("no passing Tester result matches {text}"));
                if let Some(results) = result {
                    if reference.command.is_none() {
                        row.diagnostics.push(format!(
                            "observed Tester identities for {text} (showing {} of {}): {}",
                            results.tests.len().min(20),
                            results.tests.len(),
                            results
                                .tests
                                .iter()
                                .take(20)
                                .map(|test| format!("{:?} ({})", test.id, test.status))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ));
                    }
                } else {
                    row.diagnostics
                        .push("Tester evidence is missing or bound to another candidate".into());
                }
            }
            rows.push(row);
        }
    }
    Ok(rows)
}

pub(crate) fn validate_reference_source(
    workspace: &Path,
    candidate: &str,
    reference: &TestReference,
) -> Result<()> {
    if let ReferenceMeaning::CargoLibTest { name } = reference.meaning() {
        let identity = references::RustTestIdentity::parse(&name)
            .context("Cargo library test name is malformed")?;
        let declaration = format!("fn {}(", identity.short_name());
        let output = crate::git::run_raw(
            workspace,
            &["grep", "-l", "-F", "-e", &declaration, candidate, "--", "."],
        )?;
        anyhow::ensure!(
            output.status.success() && !output.stdout.is_empty(),
            "references missing committed Rust library test {name}"
        );
        return Ok(());
    }
    if reference.path.is_empty() && reference.command.is_some() {
        return Ok(());
    }
    let output = crate::git::run_raw(
        workspace,
        &["show", &format!("{candidate}:{}", reference.path)],
    )?;
    anyhow::ensure!(
        output.status.success(),
        "references missing committed test path {}",
        reference.path
    );
    anyhow::ensure!(
        reference.selector.is_none()
            || reference.source_contains_selector(&String::from_utf8_lossy(&output.stdout)),
        "references test identifier {:?}, which is absent from {}",
        reference.selector,
        reference.path
    );
    Ok(())
}

#[cfg(test)]
mod cargo_lib_reference_tests {
    use super::*;

    fn assess_virtual_reference(repo: &Path, candidate: &str, id: &str) -> bool {
        let contract = work_model::WriterCompletionContract {
            version: work_model::WRITER_COMPLETION_CONTRACT_VERSION,
            requirements: vec![work_model::WriterCompletionRequirement {
                responsibility: None,
                id: "behavior:custom:b1".into(),
                kind: work_model::WriterCompletionRequirementKind::Behavior,
                source: "behaviors.md#custom:b1".into(),
                requirement: "Verify the library test.".into(),
                command: None,
                verification_refs: vec!["cargo test --lib verifies_assignment".into()],
            }],
        };
        let results: crate::tester::TesterResults = serde_json::from_value(serde_json::json!({
            "candidate_commit": candidate, "commands": [],
            "tests": [{"id": id, "test_harness": "cargo-nextest", "status": "pass", "duration_ms": 1, "failure_excerpt": null}],
            "summary": {"total": 1, "pass": 1, "fail": 0, "skipped": 0}, "error": null
        })).unwrap();
        assess_references(
            repo,
            "custom",
            "attempt-1",
            candidate,
            &contract,
            Some(&results),
            None,
            &[],
            &[],
        )
        .unwrap()[0]
            .satisfied
    }

    fn virtual_workspace_fixture() -> (tempfile::TempDir, String) {
        let repo = tempfile::tempdir().unwrap();
        crate::git::run(repo.path(), &["init", "-q"], "create workspace fixture").unwrap();
        crate::git::run(
            repo.path(),
            &["config", "user.email", "test@example.com"],
            "configure fixture",
        )
        .unwrap();
        crate::git::run(
            repo.path(),
            &["config", "user.name", "Test"],
            "configure fixture",
        )
        .unwrap();
        let member = repo.path().join("crates/model-gateway");
        std::fs::create_dir_all(member.join("src")).unwrap();
        std::fs::write(
            repo.path().join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/model-gateway\"]\nresolver = \"2\"\n",
        )
        .unwrap();
        std::fs::write(
            member.join("Cargo.toml"),
            "[package]\nname = \"model-gateway\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        std::fs::write(
            member.join("src/lib.rs"),
            "#[test]\nfn verifies_assignment() {}\n",
        )
        .unwrap();
        crate::git::run(repo.path(), &["add", "."], "stage fixture").unwrap();
        crate::git::run(
            repo.path(),
            &["commit", "-qm", "Add workspace fixture"],
            "commit fixture",
        )
        .unwrap();
        let candidate =
            crate::git::run_stdout(repo.path(), &["rev-parse", "HEAD"], "read fixture").unwrap();
        (repo, candidate)
    }

    #[test]
    fn cargo_lib_reference_selects_virtual_workspace_member() {
        let (repo, candidate) = virtual_workspace_fixture();
        assert!(
            candidate_library_target_with_metadata(
                repo.path(),
                &candidate,
                "verifies_assignment",
                || None,
            )
            .is_none()
        );
        let reference =
            references::parse_test_reference("cargo test --lib verifies_assignment").unwrap();
        validate_reference_source(repo.path(), &candidate, &reference).unwrap();
        let library =
            candidate_library_target(repo.path(), &candidate, "verifies_assignment").unwrap();
        assert_eq!(
            (&*library.package, &*library.library),
            ("model-gateway", "model_gateway")
        );
        for (id, accepted) in [
            ("model-gateway::model_gateway$verifies_assignment", true),
            ("verifies_assignment", false),
            ("model-gateway$verifies_assignment", false),
            ("model-gateway::model-gateway$verifies_assignment", false),
            (
                "model-gateway::integration/model_gateway$verifies_assignment",
                false,
            ),
            (
                "model-gateway::bin/model_gateway$verifies_assignment",
                false,
            ),
        ] {
            let results: crate::tester::TesterResults = serde_json::from_value(serde_json::json!({
                "candidate_commit": candidate, "commands": [],
                "tests": [{"id": id, "test_harness": "cargo-nextest", "status": "pass", "duration_ms": 1, "failure_excerpt": null}],
                "summary": {"total": 1, "pass": 1, "fail": 0, "skipped": 0}, "error": null
            })).unwrap();
            assert_eq!(
                reference_is_satisfied(&reference, &results, None, Some(&library)),
                accepted,
                "{id}"
            );
            assert_eq!(
                assess_virtual_reference(repo.path(), &candidate, id),
                accepted,
                "assess_references: {id}"
            );
        }

        let member = repo.path().join("crates/model-gateway");
        std::fs::write(member.join("Cargo.toml"), "[package]\nname = \"model-gateway\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[[bin]]\nname = \"tool\"\npath = \"src/tool.rs\"\n").unwrap();
        std::fs::write(member.join("src/lib.rs"), "pub fn library() {}\n").unwrap();
        std::fs::write(
            member.join("src/tool.rs"),
            "#[test]\nfn verifies_assignment() {}\nfn main() {}\n",
        )
        .unwrap();
        crate::git::run(repo.path(), &["add", "."], "stage binary-only fixture").unwrap();
        crate::git::run(
            repo.path(),
            &["commit", "-qm", "Move test to binary"],
            "commit binary-only fixture",
        )
        .unwrap();
        let binary_only =
            crate::git::run_stdout(repo.path(), &["rev-parse", "HEAD"], "read fixture").unwrap();
        assert!(!assess_virtual_reference(
            repo.path(),
            &binary_only,
            "model-gateway::model_gateway$verifies_assignment"
        ));
    }

    #[test]
    fn cargo_lib_reference_rejects_unbound_workspace_member() {
        let (repo, candidate) = virtual_workspace_fixture();
        let member = repo.path().join("crates/model-gateway");
        let native = "model-gateway::model_gateway$verifies_assignment";
        assert!(candidate_library_target(repo.path(), &candidate, "verifies_assignment").is_some());
        for (path, dirty) in [
            (
                repo.path().join("Cargo.toml"),
                "[workspace]\nmembers = []\n",
            ),
            (
                member.join("Cargo.toml"),
                "[package]\nname = \"changed\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
            ),
            (member.join("src/lib.rs"), "#[test]\nfn changed() {}\n"),
        ] {
            let original = std::fs::read(&path).unwrap();
            std::fs::write(&path, dirty).unwrap();
            assert!(
                candidate_library_target(repo.path(), &candidate, "verifies_assignment").is_none(),
                "{}",
                path.display()
            );
            assert!(!assess_virtual_reference(repo.path(), &candidate, native));
            std::fs::write(&path, original).unwrap();
        }
        std::fs::write(
            member.join("src/lib.rs"),
            "mod helper;\n#[test]\nfn verifies_assignment() { helper::run(); }\n",
        )
        .unwrap();
        std::fs::write(member.join("src/helper.rs"), "pub fn run() {}\n").unwrap();
        crate::git::run(repo.path(), &["add", "."], "stage module fixture").unwrap();
        crate::git::run(
            repo.path(),
            &["commit", "-qm", "Add library helper"],
            "commit module fixture",
        )
        .unwrap();
        let module_candidate =
            crate::git::run_stdout(repo.path(), &["rev-parse", "HEAD"], "read fixture").unwrap();
        assert!(assess_virtual_reference(
            repo.path(),
            &module_candidate,
            native
        ));
        std::fs::write(member.join("src/helper.rs"), "pub fn run() { panic!(); }\n").unwrap();
        assert!(!assess_virtual_reference(
            repo.path(),
            &module_candidate,
            native
        ));
        crate::git::run(
            repo.path(),
            &["restore", "crates/model-gateway/src/helper.rs"],
            "restore fixture",
        )
        .unwrap();
        std::fs::write(
            member.join("src/lib.rs"),
            "#[path = \"../shared.rs\"] mod helper;\n#[test]\nfn verifies_assignment() { helper::run(); }\n",
        )
        .unwrap();
        std::fs::write(member.join("shared.rs"), "pub fn run() {}\n").unwrap();
        crate::git::run(repo.path(), &["add", "."], "stage external module fixture").unwrap();
        crate::git::run(
            repo.path(),
            &["commit", "-qm", "Use shared library source"],
            "commit external module fixture",
        )
        .unwrap();
        let shared_candidate =
            crate::git::run_stdout(repo.path(), &["rev-parse", "HEAD"], "read fixture").unwrap();
        assert!(assess_virtual_reference(
            repo.path(),
            &shared_candidate,
            native
        ));
        std::fs::write(member.join("shared.rs"), "pub fn run() { panic!(); }\n").unwrap();
        assert!(!assess_virtual_reference(
            repo.path(),
            &shared_candidate,
            native
        ));
        crate::git::run(
            repo.path(),
            &["restore", "crates/model-gateway/shared.rs"],
            "restore fixture",
        )
        .unwrap();
        std::fs::write(member.join("src/lib.rs"), "#[cfg_attr(unix, path = \"../shared.rs\")] mod helper;\n#[test]\nfn verifies_assignment() { helper::run(); }\n").unwrap();
        crate::git::run(
            repo.path(),
            &["add", "."],
            "stage conditional source fixture",
        )
        .unwrap();
        crate::git::run(
            repo.path(),
            &["commit", "-qm", "Use conditional library source"],
            "commit conditional source fixture",
        )
        .unwrap();
        let conditional_candidate =
            crate::git::run_stdout(repo.path(), &["rev-parse", "HEAD"], "read fixture").unwrap();
        assert!(assess_virtual_reference(
            repo.path(),
            &conditional_candidate,
            native
        ));
        std::fs::write(member.join("shared.rs"), "pub fn run() { panic!(); }\n").unwrap();
        assert!(!assess_virtual_reference(
            repo.path(),
            &conditional_candidate,
            native
        ));
        crate::git::run(
            repo.path(),
            &["restore", "crates/model-gateway/shared.rs"],
            "restore fixture",
        )
        .unwrap();

        std::fs::write(member.join("src/lib.rs"), "include!(\"../shared.rs\");\n").unwrap();
        std::fs::write(
            member.join("shared.rs"),
            "#[test]\nfn verifies_assignment() {}\n",
        )
        .unwrap();
        crate::git::run(repo.path(), &["add", "."], "stage included test fixture").unwrap();
        crate::git::run(
            repo.path(),
            &["commit", "-qm", "Include library test"],
            "commit included test fixture",
        )
        .unwrap();
        let included_candidate =
            crate::git::run_stdout(repo.path(), &["rev-parse", "HEAD"], "read fixture").unwrap();
        assert!(assess_virtual_reference(
            repo.path(),
            &included_candidate,
            native
        ));
        std::fs::remove_file(member.join("src/lib.rs")).unwrap();
        assert!(candidate_library_target(repo.path(), &candidate, "verifies_assignment").is_none());
        crate::git::run(
            repo.path(),
            &["restore", "crates/model-gateway/src/lib.rs"],
            "restore fixture",
        )
        .unwrap();

        let second = repo.path().join("crates/other");
        std::fs::create_dir_all(second.join("src")).unwrap();
        std::fs::write(repo.path().join("Cargo.toml"), "[workspace]\nmembers = [\"crates/model-gateway\", \"crates/other\"]\nresolver = \"2\"\n").unwrap();
        std::fs::write(
            second.join("Cargo.toml"),
            "[package]\nname = \"other\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        std::fs::write(
            second.join("src/lib.rs"),
            "#[test]\nfn verifies_assignment() {}\n",
        )
        .unwrap();
        crate::git::run(repo.path(), &["add", "."], "stage ambiguous fixture").unwrap();
        crate::git::run(
            repo.path(),
            &["commit", "-qm", "Add second library"],
            "commit ambiguous fixture",
        )
        .unwrap();
        let ambiguous =
            crate::git::run_stdout(repo.path(), &["rev-parse", "HEAD"], "read fixture").unwrap();
        assert!(candidate_library_target(repo.path(), &ambiguous, "verifies_assignment").is_none());
        assert!(
            candidate_library_target_with_metadata(
                repo.path(),
                &ambiguous,
                "verifies_assignment",
                || None,
            )
            .is_none()
        );
        assert!(!assess_virtual_reference(repo.path(), &ambiguous, native));
        std::fs::write(second.join("src/other.rs"), "pub fn other() {}\n").unwrap();
        crate::git::run(repo.path(), &["add", "."], "stage alternate library source").unwrap();
        crate::git::run(
            repo.path(),
            &["commit", "-qm", "Add alternate library source"],
            "commit alternate library source",
        )
        .unwrap();
        let ambiguous_with_alternate =
            crate::git::run_stdout(repo.path(), &["rev-parse", "HEAD"], "read fixture").unwrap();
        std::fs::write(second.join("Cargo.toml"), "[package]\nname = \"other\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[lib]\npath = \"src/other.rs\"\n").unwrap();
        assert!(!assess_virtual_reference(
            repo.path(),
            &ambiguous_with_alternate,
            native
        ));
        std::fs::write(second.join("Cargo.toml"), "[package]\nname = \"other\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[lib] # external test source\npath = \"../../shared-other.rs\"\n").unwrap();
        std::fs::write(second.join("src/lib.rs"), "pub fn old_library() {}\n").unwrap();
        std::fs::write(
            repo.path().join("shared-other.rs"),
            "#[test]\nfn verifies_assignment() {}\n",
        )
        .unwrap();
        crate::git::run(repo.path(), &["add", "."], "stage external second library").unwrap();
        crate::git::run(
            repo.path(),
            &["commit", "-qm", "Add external second library"],
            "commit external second library",
        )
        .unwrap();
        let external_ambiguous =
            crate::git::run_stdout(repo.path(), &["rev-parse", "HEAD"], "read fixture").unwrap();
        assert!(!assess_virtual_reference(
            repo.path(),
            &external_ambiguous,
            native
        ));
        std::fs::write(second.join("Cargo.toml"), "[package]\nname = \"other\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[lib]\npath = \"src/other.rs\"\n").unwrap();
        assert!(!assess_virtual_reference(
            repo.path(),
            &external_ambiguous,
            native
        ));
        std::fs::write(
            second.join("Cargo.toml"),
            "[package]\nname = \"other\"\nversion = \"0.2.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        std::fs::write(second.join("src/lib.rs"), "#[test]\nfn changed() {}\n").unwrap();
        let unique = repo.path().join("Cargo.toml");
        std::fs::write(&unique, "[workspace]\nmembers = [\"crates/model-gateway\", \"crates/other\"]\nresolver = \"2\"\n").unwrap();
        crate::git::run(repo.path(), &["add", "."], "stage unique fixture").unwrap();
        crate::git::run(
            repo.path(),
            &["commit", "-qm", "Remove duplicate test"],
            "commit unique fixture",
        )
        .unwrap();
        let unique_candidate =
            crate::git::run_stdout(repo.path(), &["rev-parse", "HEAD"], "read fixture").unwrap();
        std::fs::write(
            second.join("Cargo.toml"),
            "[package]\nname = \"other\"\nversion = \"0.3.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        assert!(assess_virtual_reference(
            repo.path(),
            &unique_candidate,
            native
        ));
    }

    #[test]
    fn cargo_lib_test_reference_binds_committed_source_and_native_result() {
        let repo = tempfile::tempdir().unwrap();
        crate::git::run(repo.path(), &["init", "-q"], "create reference fixture").unwrap();
        crate::git::run(
            repo.path(),
            &["config", "user.email", "test@example.com"],
            "configure reference fixture",
        )
        .unwrap();
        crate::git::run(
            repo.path(),
            &["config", "user.name", "Test"],
            "configure reference fixture",
        )
        .unwrap();
        std::fs::create_dir_all(repo.path().join("src")).unwrap();
        std::fs::write(
            repo.path().join("src/feature.rs"),
            "#[test]\nfn verifies_assignment() {}\n",
        )
        .unwrap();
        crate::git::run(
            repo.path(),
            &["add", "src/feature.rs"],
            "stage reference fixture",
        )
        .unwrap();
        crate::git::run(
            repo.path(),
            &["commit", "-qm", "Add reference fixture"],
            "commit reference fixture",
        )
        .unwrap();
        let candidate =
            crate::git::run_stdout(repo.path(), &["rev-parse", "HEAD"], "read fixture").unwrap();
        let reference =
            references::parse_test_reference("`cargo test --lib verifies_assignment`").unwrap();
        validate_reference_source(repo.path(), &candidate, &reference).unwrap();
        let missing = references::parse_test_reference("cargo test --lib absent_test").unwrap();
        assert!(validate_reference_source(repo.path(), &candidate, &missing).is_err());

        let results: crate::tester::TesterResults = serde_json::from_value(serde_json::json!({
            "candidate_commit": candidate,
            "commands": [],
            "tests": [{"id":"fluent::fluent$feature::verifies_assignment","test_harness":"cargo-nextest","status":"pass","duration_ms":1,"failure_excerpt":null}],
            "summary": {"total":1,"pass":1,"fail":0,"skipped":0},
            "error": null
        }))
        .unwrap();
        let library = CandidateLibraryTarget {
            package: "fluent".into(),
            library: "fluent".into(),
        };
        assert!(reference_is_satisfied(
            &reference,
            &results,
            None,
            Some(&library)
        ));
        let mut failed = results;
        failed.tests[0].status = "fail".into();
        assert!(!reference_is_satisfied(
            &reference,
            &failed,
            None,
            Some(&library)
        ));
        failed.tests[0].status = "pass".into();
        failed.tests[0].test_harness = "shell-harness".into();
        assert!(!reference_is_satisfied(
            &reference,
            &failed,
            None,
            Some(&library)
        ));
    }

    #[test]
    fn cargo_lib_test_reference_matches_nextest_library_target() {
        let reference =
            references::parse_test_reference("cargo test --lib verifies_assignment").unwrap();
        let library = CandidateLibraryTarget {
            package: "fluent".into(),
            library: "fluent".into(),
        };
        let result = |id: &str, status: &str| -> crate::tester::TesterResults {
            serde_json::from_value(serde_json::json!({
                "candidate_commit": "candidate",
                "commands": [],
                "tests": [{"id": id, "test_harness": "cargo-nextest", "status": status, "duration_ms": 1, "failure_excerpt": null}],
                "summary": {"total": 1, "pass": 1, "fail": 0, "skipped": 0},
                "error": null
            })).unwrap()
        };
        assert!(reference_is_satisfied(
            &reference,
            &result("fluent::fluent$feature::verifies_assignment", "pass"),
            None,
            Some(&library)
        ));
        for id in [
            "feature::verifies_assignment",
            "fluent$feature::verifies_assignment",
            "another::another$feature::verifies_assignment",
            "fluent::binary$feature::verifies_assignment",
            "fluent::bin/fluent$feature::verifies_assignment",
            "fluent::integration/fluent$feature::verifies_assignment",
        ] {
            assert!(
                !reference_is_satisfied(&reference, &result(id, "pass"), None, Some(&library)),
                "{id}"
            );
        }
        assert!(!reference_is_satisfied(
            &reference,
            &result("fluent::fluent$feature::verifies_assignment", "fail"),
            None,
            Some(&library)
        ));
        let mut mixed = result("fluent::fluent$feature::verifies_assignment", "pass");
        mixed
            .tests
            .extend(result("fluent::binary$feature::verifies_assignment", "fail").tests);
        assert!(reference_is_satisfied(
            &reference,
            &mixed,
            None,
            Some(&library)
        ));
    }

    #[test]
    fn cargo_lib_reference_uses_candidate_package_and_library_target() {
        let repo = tempfile::tempdir().unwrap();
        crate::git::run(repo.path(), &["init", "-q"], "create candidate fixture").unwrap();
        crate::git::run(
            repo.path(),
            &["config", "user.email", "test@example.com"],
            "configure fixture",
        )
        .unwrap();
        crate::git::run(
            repo.path(),
            &["config", "user.name", "Test"],
            "configure fixture",
        )
        .unwrap();
        std::fs::create_dir_all(repo.path().join("src")).unwrap();
        std::fs::write(repo.path().join("Cargo.toml"), "[package]\nname = \"other-package\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[lib]\nname = \"other_library\"\n").unwrap();
        std::fs::write(
            repo.path().join("src/lib.rs"),
            "#[test]\nfn verifies_assignment() {}\n",
        )
        .unwrap();
        crate::git::run(
            repo.path(),
            &["add", "Cargo.toml", "src/lib.rs"],
            "stage fixture",
        )
        .unwrap();
        crate::git::run(
            repo.path(),
            &["commit", "-qm", "Add candidate fixture"],
            "commit fixture",
        )
        .unwrap();
        let candidate =
            crate::git::run_stdout(repo.path(), &["rev-parse", "HEAD"], "read fixture").unwrap();
        let unavailable = candidate_library_target_with_metadata(
            repo.path(),
            &candidate,
            "verifies_assignment",
            || None,
        )
        .expect("committed package-root library resolves without Cargo metadata");
        assert_eq!(
            (&*unavailable.package, &*unavailable.library),
            ("other-package", "other_library")
        );
        let contract = work_model::WriterCompletionContract {
            version: work_model::WRITER_COMPLETION_CONTRACT_VERSION,
            requirements: vec![work_model::WriterCompletionRequirement {
                responsibility: None,
                id: "behavior:custom:b1".into(),
                kind: work_model::WriterCompletionRequirementKind::Behavior,
                source: "behaviors.md#custom:b1".into(),
                requirement: "THE SYSTEM SHALL verify the library test.".into(),
                command: None,
                verification_refs: vec!["cargo test --lib verifies_assignment".into()],
            }],
        };
        for (id, accepted) in [
            ("other-package::other_library$verifies_assignment", true),
            ("verifies_assignment", false),
            ("other-package$verifies_assignment", false),
            ("other-package::integration$verifies_assignment", false),
            (
                "other-package::bin/other_library$verifies_assignment",
                false,
            ),
            ("fluent::fluent$verifies_assignment", false),
        ] {
            let results: crate::tester::TesterResults = serde_json::from_value(serde_json::json!({
                "candidate_commit": candidate,
                "commands": [],
                "tests": [{"id": id, "test_harness": "cargo-nextest", "status": "pass", "duration_ms": 1, "failure_excerpt": null}],
                "summary": {"total": 1, "pass": 1, "fail": 0, "skipped": 0},
                "error": null
            })).unwrap();
            let reference =
                references::parse_test_reference("cargo test --lib verifies_assignment").unwrap();
            assert_eq!(
                reference_is_satisfied(&reference, &results, None, Some(&unavailable)),
                accepted,
                "metadata unavailable: {id}"
            );
            let rows = assess_references(
                repo.path(),
                "custom",
                "attempt-1",
                &candidate,
                &contract,
                Some(&results),
                None,
                &[],
                &[],
            )
            .unwrap();
            assert_eq!(rows[0].satisfied, accepted, "{id}");
        }
        std::fs::write(repo.path().join("Cargo.toml"), "[package]\nname = \"other-package\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[lib]\nname = \"uncommitted_library\"\n").unwrap();
        let results: crate::tester::TesterResults = serde_json::from_value(serde_json::json!({
            "candidate_commit": candidate,
            "commands": [],
            "tests": [{"id": "other-package::uncommitted_library$verifies_assignment", "test_harness": "cargo-nextest", "status": "pass", "duration_ms": 1, "failure_excerpt": null}],
            "summary": {"total": 1, "pass": 1, "fail": 0, "skipped": 0},
            "error": null
        })).unwrap();
        let rows = assess_references(
            repo.path(),
            "custom",
            "attempt-1",
            &candidate,
            &contract,
            Some(&results),
            None,
            &[],
            &[],
        )
        .unwrap();
        assert!(
            !rows[0].satisfied,
            "uncommitted target metadata was credited"
        );
        let mut committed_results = results;
        committed_results.tests[0].id = "other-package::other_library$verifies_assignment".into();
        let rows = assess_references(
            repo.path(),
            "custom",
            "attempt-1",
            &candidate,
            &contract,
            Some(&committed_results),
            None,
            &[],
            &[],
        )
        .unwrap();
        assert!(
            rows[0].satisfied,
            "the committed library target was lost after a worktree edit"
        );
        std::fs::write(
            repo.path().join("src/lib.rs"),
            "#[test]\nfn verifies_assignment() { panic!(); }\n",
        )
        .unwrap();
        assert!(
            candidate_library_target_with_metadata(
                repo.path(),
                &candidate,
                "verifies_assignment",
                || None,
            )
            .is_none()
        );
        let rows = assess_references(
            repo.path(),
            "custom",
            "attempt-1",
            &candidate,
            &contract,
            Some(&committed_results),
            None,
            &[],
            &[],
        )
        .unwrap();
        assert!(!rows[0].satisfied, "a dirty library source was credited");
    }
}

#[derive(Debug)]
struct CandidateLibraryTarget {
    package: String,
    library: String,
}

fn candidate_library_target(
    workspace: &Path,
    candidate: &str,
    name: &str,
) -> Option<CandidateLibraryTarget> {
    candidate_library_target_with_metadata(workspace, candidate, name, || {
        let output = Command::new("cargo")
            .args([
                "metadata",
                "--no-deps",
                "--format-version",
                "1",
                "--offline",
                "--manifest-path",
            ])
            .arg(workspace.join("Cargo.toml"))
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| serde_json::from_slice(&output.stdout).ok())?
    })
}

fn candidate_library_target_with_metadata(
    workspace: &Path,
    candidate: &str,
    name: &str,
    load_metadata: impl FnOnce() -> Option<serde_json::Value>,
) -> Option<CandidateLibraryTarget> {
    fn committed_file(workspace: &Path, candidate: &str, path: &Path) -> Option<Vec<u8>> {
        let relative = path.strip_prefix(workspace).ok()?.to_str()?;
        let committed =
            crate::git::run_raw(workspace, &["show", &format!("{candidate}:{relative}")]).ok()?;
        committed.status.success().then_some(committed.stdout)
    }
    fn bound_file(workspace: &Path, candidate: &str, path: &Path) -> Option<()> {
        (std::fs::read(path).ok()? == committed_file(workspace, candidate, path)?).then_some(())
    }
    fn workspace_path(workspace: &Path, path: &Path) -> Option<PathBuf> {
        let path = path.canonicalize().ok()?;
        let relative = path.strip_prefix(workspace.canonicalize().ok()?).ok()?;
        Some(workspace.join(relative))
    }
    fn library_sources(
        workspace: &Path,
        candidate: &str,
        roots: &[PathBuf],
    ) -> Option<Vec<PathBuf>> {
        fn literal_paths<'a>(
            text: &'a str,
            marker: &str,
            separator: &str,
            end: &str,
        ) -> Option<Vec<&'a str>> {
            text.match_indices(marker)
                .map(|(index, _)| {
                    let rest = text[index + marker.len()..].trim_start();
                    let rest = rest
                        .strip_prefix(separator)?
                        .trim_start()
                        .strip_prefix('"')?;
                    let (path, rest) = rest.split_once('"')?;
                    rest.trim_start().starts_with(end).then_some(path)
                })
                .collect()
        }
        fn attribute_paths(text: &str) -> Option<Vec<&str>> {
            let mut paths = Vec::new();
            let mut remaining = text;
            while let Some(index) = remaining.find("#[") {
                let attribute = &remaining[index + 2..];
                let end = attribute.find(']')?;
                let body = &attribute[..end];
                for (offset, _) in body.match_indices("path") {
                    let preceding = body[..offset].chars().next_back();
                    if preceding.is_some_and(|c| c == '_' || c.is_alphanumeric()) {
                        continue;
                    }
                    let rest = body[offset + 4..].trim_start();
                    if let Some(rest) = rest.strip_prefix('=') {
                        let rest = rest.trim_start().strip_prefix('"')?;
                        let (path, _) = rest.split_once('"')?;
                        paths.push(path);
                    }
                }
                remaining = &attribute[end + 1..];
            }
            Some(paths)
        }
        let mut pending = roots.to_vec();
        let mut seen = std::collections::HashSet::new();
        while let Some(source) = pending.pop() {
            if !seen.insert(source.clone()) {
                continue;
            }
            let bytes = committed_file(workspace, candidate, &source)?;
            let text = std::str::from_utf8(&bytes).ok()?;
            for path in attribute_paths(text)?
                .into_iter()
                .chain(literal_paths(text, "include!", "(", ")")?)
            {
                let path = source.parent()?.join(path);
                pending.push(workspace_path(workspace, &path)?);
            }
        }
        Some(seen.into_iter().collect())
    }
    fn committed_explicit_library_source(
        workspace: &Path,
        candidate: &str,
        manifest: &Path,
    ) -> Option<Option<PathBuf>> {
        let bytes = committed_file(workspace, candidate, manifest)?;
        let text = std::str::from_utf8(&bytes).ok()?;
        let mut in_library = false;
        for line in text.lines().map(str::trim) {
            if line.starts_with('[') {
                in_library = line.starts_with("[lib]");
            } else if in_library && let Some((key, value)) = line.split_once('=') {
                if key.trim() == "path" {
                    let value = value.trim();
                    let quote = value.chars().next()?;
                    if quote != '"' && quote != '\'' {
                        return None;
                    }
                    let path = value[1..].split_once(quote)?.0;
                    return workspace_path(workspace, &manifest.parent()?.join(path)).map(Some);
                }
            }
        }
        Some(None)
    }
    fn committed_root_library(
        workspace: &Path,
        candidate: &str,
        declaration: &str,
    ) -> Option<CandidateLibraryTarget> {
        let manifest = committed_file(workspace, candidate, &workspace.join("Cargo.toml"))?;
        let manifest = std::str::from_utf8(&manifest).ok()?;
        // A dirty workspace manifest can change member selection. This
        // fallback applies only to a standalone package at the repository root.
        if manifest
            .lines()
            .any(|line| line.trim().starts_with("[workspace]"))
        {
            return None;
        }
        let mut section = "";
        let (mut package, mut library, mut source, mut autolib) = (None, None, None, None);
        let mut declared_library = false;
        for line in manifest.lines().map(str::trim) {
            if line.starts_with('[') {
                section = line.split('#').next()?.trim();
                declared_library |= section == "[lib]";
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            if section == "[package]" && key.trim() == "autolib" {
                if autolib.is_some() {
                    return None;
                }
                autolib = Some(match value.split('#').next()?.trim() {
                    "true" => true,
                    "false" => false,
                    _ => return None,
                });
                continue;
            }
            let slot = match (section, key.trim()) {
                ("[package]", "name") => &mut package,
                ("[lib]", "name") => &mut library,
                ("[lib]", "path") => &mut source,
                _ => continue,
            };
            if slot.is_some() {
                return None;
            }
            *slot = Some(manifest_string(value.trim())?);
        }
        if autolib == Some(false) && !declared_library {
            return None;
        }
        let package: String = package?;
        let library = library.unwrap_or_else(|| package.replace('-', "_"));
        let source = source.unwrap_or_else(|| "src/lib.rs".into());
        let source = Path::new(&source);
        if !source
            .extension()
            .is_some_and(|extension| extension == "rs")
            || !source
                .components()
                .all(|component| matches!(component, Component::Normal(_)))
        {
            return None;
        }
        let source = workspace.join(source);
        let sources = library_sources(workspace, candidate, &[source])?;
        for path in &sources {
            bound_file(workspace, candidate, path)?;
        }
        sources
            .iter()
            .any(|path| {
                committed_file(workspace, candidate, path)
                    .is_some_and(|bytes| String::from_utf8_lossy(&bytes).contains(declaration))
            })
            .then_some(CandidateLibraryTarget { package, library })
    }
    let test = references::RustTestIdentity::parse(name)?;
    let declaration = format!("fn {}(", test.short_name());
    if bound_file(workspace, candidate, &workspace.join("Cargo.toml")).is_none() {
        // Resolve a standalone package from the commit when an unrelated
        // worktree manifest edit would make live Cargo metadata misleading.
        return committed_root_library(workspace, candidate, &declaration);
    }
    let Some(metadata) = load_metadata() else {
        return committed_root_library(workspace, candidate, &declaration);
    };
    let members = metadata.get("workspace_members")?.as_array()?;
    let mut matches = Vec::new();
    for package in metadata.get("packages")?.as_array()? {
        if !members.contains(package.get("id")?) {
            continue;
        }
        let manifest = Path::new(package.get("manifest_path")?.as_str()?);
        if bound_file(workspace, candidate, manifest).is_none() {
            // Live Cargo metadata can hide a committed declaring library by
            // redirecting this member's target. Inspect committed source
            // before treating a dirty member as unrelated.
            let member = manifest.parent()?.strip_prefix(workspace).ok()?.to_str()?;
            let declarations = crate::git::run_raw(
                workspace,
                &[
                    "grep",
                    "-l",
                    "-F",
                    "-e",
                    &declaration,
                    candidate,
                    "--",
                    member,
                ],
            )
            .ok()?;
            if !declarations.status.success() && declarations.status.code() != Some(1) {
                return None;
            }
            let prefix = format!("{candidate}:");
            if String::from_utf8(declarations.stdout)
                .ok()?
                .lines()
                .filter_map(|line| line.strip_prefix(&prefix))
                .any(|path| Path::new(path).extension().is_some_and(|ext| ext == "rs"))
            {
                return None;
            }
            if let Some(source) = committed_explicit_library_source(workspace, candidate, manifest)?
            {
                let sources = library_sources(workspace, candidate, &[source])?;
                if sources.iter().any(|path| {
                    committed_file(workspace, candidate, path)
                        .is_some_and(|bytes| String::from_utf8_lossy(&bytes).contains(&declaration))
                }) {
                    return None;
                }
            }
        }
        let other_targets = package
            .get("targets")?
            .as_array()?
            .iter()
            .filter(|target| {
                !target["kind"]
                    .as_array()
                    .is_some_and(|kinds| kinds.iter().any(|kind| kind == "lib"))
            })
            .map(|target| {
                workspace_path(workspace, Path::new(target.get("src_path")?.as_str()?))?
                    .strip_prefix(workspace)
                    .ok()
                    .map(Path::to_path_buf)
            })
            .collect::<Option<Vec<_>>>()?;
        for library in package.get("targets")?.as_array()?.iter().filter(|target| {
            target["kind"]
                .as_array()
                .is_some_and(|kinds| kinds.iter().any(|kind| kind == "lib"))
        }) {
            let source = workspace_path(workspace, Path::new(library.get("src_path")?.as_str()?))?;
            let source_relative = source.strip_prefix(workspace).ok()?;
            let source_dir = source.parent()?;
            let library_files = if source.starts_with(manifest.parent()?) {
                let relative_dir = source_dir.strip_prefix(workspace).ok()?.to_str()?;
                let files = crate::git::run_raw(
                    workspace,
                    &[
                        "ls-tree",
                        "-r",
                        "--name-only",
                        candidate,
                        "--",
                        relative_dir,
                    ],
                )
                .ok()?;
                if !files.status.success() {
                    return None;
                }
                String::from_utf8(files.stdout)
                    .ok()?
                    .lines()
                    .filter(|relative| {
                        let path = Path::new(relative);
                        path.extension().is_some_and(|extension| extension == "rs")
                            && (path == source_relative
                                || path
                                    .file_name()
                                    .is_none_or(|file| file != "lib.rs" && file != "main.rs"))
                            && !path.components().any(|part| part.as_os_str() == "bin")
                            && !other_targets.iter().any(|target| {
                                target == path
                                    || target.file_stem().is_some_and(|stem| {
                                        path.starts_with(target.with_file_name(stem))
                                    })
                            })
                    })
                    .map(|relative| workspace.join(relative))
                    .collect::<Vec<_>>()
            } else {
                vec![source.to_path_buf()]
            };
            let sources = library_sources(workspace, candidate, &library_files)?;
            let declared = sources.iter().any(|path| {
                committed_file(workspace, candidate, path)
                    .is_some_and(|content| String::from_utf8_lossy(&content).contains(&declaration))
            });
            if declared {
                bound_file(workspace, candidate, manifest)?;
                for path in &sources {
                    bound_file(workspace, candidate, path)?;
                }
                matches.push(CandidateLibraryTarget {
                    package: package.get("name")?.as_str()?.to_owned(),
                    library: library.get("name")?.as_str()?.to_owned(),
                });
            }
        }
    }
    (matches.len() == 1)
        .then(|| matches.remove(0))
        .or_else(|| committed_root_library(workspace, candidate, &declaration))
}

fn manifest_string(value: &str) -> Option<String> {
    let quote = value.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let mut escaped = false;
    for (index, character) in value.char_indices().skip(1) {
        if character == quote && !escaped {
            let remainder = value[index + character.len_utf8()..].trim();
            if !remainder.is_empty() && !remainder.starts_with('#') {
                return None;
            }
            return if quote == '"' {
                serde_json::from_str(&value[..=index]).ok()
            } else {
                Some(value[1..index].to_owned())
            };
        }
        escaped = character == '\\' && !escaped && quote == '"';
    }
    None
}

fn reference_is_satisfied(
    reference: &TestReference,
    results: &crate::tester::TesterResults,
    canonical: Option<&CanonicalCheckAssessment>,
    candidate_library: Option<&CandidateLibraryTarget>,
) -> bool {
    let normalized = results
        .tests
        .iter()
        .map(references::normalized_result)
        .collect::<std::result::Result<Vec<_>, _>>();
    let Ok(normalized) = normalized else {
        return false;
    };
    let mut results = results.clone();
    results.tests = normalized;
    if let Some(view) = canonical {
        let command_identities = results
            .commands
            .iter()
            .map(|command| (command.command.as_str(), command.test_harness.as_str()))
            .collect::<Vec<_>>();
        for test in &mut results.tests {
            if test.status == "fail"
                && failed_test_covered_by_accepted_command(
                    &command_identities,
                    &view.accepted,
                    test,
                )
            {
                test.status = "pass".into();
            }
        }
    }
    match reference.meaning() {
        ReferenceMeaning::CargoLibTest { name } => {
            let Some(library) = candidate_library else {
                return false;
            };
            let Some(expected) = references::RustTestIdentity::parse(&name) else {
                return false;
            };
            let tests = results
                .tests
                .iter()
                .filter(|test| matches!(test.test_harness.as_str(), "cargo-nextest" | "cargo-test"))
                .filter_map(|test| {
                    references::RustTestIdentity::parse(&test.id)
                        .filter(|identity| {
                            identity.binary.is_some_and(|binary| {
                                binary == format!("{}::{}", library.package, library.library)
                            })
                        })
                        .map(|identity| (identity, test))
                })
                .collect::<Vec<_>>();
            references::rust_evidence_matches_reference(&expected, &tests)
        }
        ReferenceMeaning::Command {
            command,
            command_index,
            ..
        } => {
            let matches = results
                .commands
                .iter()
                .enumerate()
                .filter(|(index, result)| {
                    result.command == command
                        && command_index.is_none_or(|expected| expected == *index)
                })
                .collect::<Vec<_>>();
            let [(index, result)] = matches.as_slice() else {
                return false;
            };
            canonical.is_some_and(|view| {
                view.checks
                    .get(*index)
                    .is_some_and(|check| check.command == command)
                    && !view.outstanding.contains(index)
                    && (result.exit_code == 0 || view.accepted.contains(index))
            })
        }
        ReferenceMeaning::WholeScript { path, .. } => {
            let matching = results
                .tests
                .iter()
                .filter(|test| test.id == path)
                .collect::<Vec<_>>();
            !matching.is_empty() && matching.iter().all(|test| test.status == "pass")
        }
        ReferenceMeaning::NamedTest { path, name, .. } if path.ends_with(".sh") => {
            let matching = results
                .tests
                .iter()
                .filter(|test| test.id == format!("{path}::{name}"))
                .collect::<Vec<_>>();
            !matching.is_empty() && matching.iter().all(|test| test.status == "pass")
        }
        ReferenceMeaning::NamedTest { path, .. } if path.ends_with(".rs") => {
            let tests = results
                .tests
                .iter()
                .filter_map(|test| {
                    references::RustTestIdentity::parse(&test.id).map(|identity| (identity, test))
                })
                .collect::<Vec<_>>();
            reference.rust_identity().is_some_and(|expected| {
                references::rust_evidence_matches_reference(&expected, &tests)
            })
        }
        ReferenceMeaning::NamedTest { path, .. } if path.ends_with(".py") => {
            let tests = results
                .tests
                .iter()
                .filter_map(|test| {
                    references::PythonTestIdentity::parse(&test.id).map(|identity| (identity, test))
                })
                .collect::<Vec<_>>();
            reference.python_identity().is_some_and(|expected| {
                references::python_evidence_matches_reference(&expected, &tests)
            })
        }
        _ => false,
    }
}

/// Observe a saved Attempt without journal recovery, migration, or publication.
pub fn assess_current_verification(
    project_root: &Path,
    attempt: &work_model::Attempt,
) -> Result<VerificationAssessment> {
    assess_current_verification_at(
        project_root,
        attempt,
        crate::check_applicability::evaluation_time(),
    )
}

pub fn assess_current_verification_at(
    project_root: &Path,
    attempt: &work_model::Attempt,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<VerificationAssessment> {
    let output = attempt
        .tasks
        .iter()
        .rev()
        .find(|task| task.kind == TaskKind::Write && task.status == TaskStatus::Complete)
        .and_then(|task| task.output.as_ref())
        .context("no candidate")?;
    // Absence of a run for a new candidate is pending work. Historical runs
    // neither satisfy its requirements nor prevent its ordinary verification.
    if !attempt.tasks.iter().any(|task| {
        task.kind == TaskKind::Tester
            && task.status == TaskStatus::Complete
            && !task.is_failed_command_recheck()
            && task
                .review_context
                .as_ref()
                .is_some_and(|context| context.candidate_commit == output.commit)
    }) {
        return Ok(VerificationAssessment {
            candidate_commit: output.commit.clone(),
            references: match &attempt.writer_completion_contract {
                Some(contract) => assess_references(
                    &candidate_workspace_path(project_root, output),
                    &attempt.work_item_id,
                    &attempt.id,
                    &output.commit,
                    contract,
                    None,
                    None,
                    &[],
                    &[],
                )?,
                None => vec![],
            },
            commands: vec![],
        });
    }
    let canonical = assess_canonical_checks_at(project_root, attempt, now)?;
    let bytes = fs::read(&canonical.results_path)?;
    let results: crate::tester::TesterResults = serde_json::from_slice(&bytes)?;
    let bindings = vec![work_model::BoundRecheckArtifact {
        path: canonical
            .results_path
            .strip_prefix(project_root)?
            .to_string_lossy()
            .into_owned(),
        digest: format!("sha256:{:x}", Sha256::digest(&bytes)),
    }];
    // Merge accepted native observations into a view for named-test reference
    // matching only. The original results remain the canonical record for
    // command-level checks.
    let native_tests =
        accepted_native_observation_tests(project_root, attempt, &output.commit, &canonical, now);
    let results_with_native = if native_tests.is_empty() {
        results.clone()
    } else {
        let mut augmented = results.clone();
        augmented.tests.extend(native_tests);
        augmented
    };
    let mut references = match &attempt.writer_completion_contract {
        Some(contract) => assess_references(
            &candidate_workspace_path(project_root, output),
            &attempt.work_item_id,
            &attempt.id,
            &output.commit,
            contract,
            Some(&results_with_native),
            Some(&canonical),
            &bindings,
            &accepted_legacy_commands(project_root, attempt, now),
        )?,
        None => vec![],
    };
    let mut commands = Vec::new();
    for (index, check) in canonical.checks.iter().enumerate() {
        let mut supporting = bindings.clone();
        supporting.extend(canonical.baseline.iter().cloned());
        supporting.extend(
            canonical
                .review_bindings
                .get(&index)
                .into_iter()
                .flatten()
                .cloned(),
        );
        if let work_model::RequiredCheckSource::CanonicalTester {
            manifest,
            logs,
            reconstruction,
            ..
        } = &check.source
        {
            supporting.push(manifest.clone());
            supporting.extend(logs.clone());
            supporting.extend(reconstruction.clone());
        }
        if canonical.accepted.contains(&index) {
            if let Some(recovery) = attempt
                .evidence_recoveries
                .iter()
                .rev()
                .find(|recovery| recovery.required_check.as_ref() == Some(check))
            {
                supporting.push(work_model::BoundRecheckArtifact {
                    path: recovery.attachment.snapshot_path.clone(),
                    digest: recovery.attachment.digest.clone(),
                });
                for target in &recovery.targets {
                    supporting.push(
                        bound_recovery_artifact(project_root, &target.prior_review_artifact)?.0,
                    );
                    if let Some(task) = attempt
                        .tasks
                        .iter()
                        .find(|task| target.review_task_id.as_deref() == Some(task.id.as_str()))
                    {
                        if let Some(path) = review_artifact_path(task) {
                            supporting.push(bound_recovery_artifact(project_root, &path)?.0);
                        }
                    }
                }
            }
        }
        let satisfied = !canonical.outstanding.contains(&index);
        commands.push(RequirementAssessment {
            id: check.id.clone(), source: "canonical Tester".into(), check: Some(check.clone()),
            meaning: Some(ReferenceMeaning::Command { command: check.command.clone(), command_index: Some(index), description: None }),
            satisfied, cause: (!satisfied).then_some(VerificationCause::Unknown),
            owner: if canonical.accepted.contains(&index) { work_model::CompletionResponsibility::Host } else { work_model::CompletionResponsibility::Tester },
            next_action: (!satisfied).then(|| "Inspect the retained command logs and establish the cause before authorizing candidate correction.".into()),
            bindings: supporting, diagnostics: if canonical.preexisting.contains(&index) {
                vec!["The command exit is explained by matching pre-write baseline failures; it is not passing evidence for an explicit reference.".into()]
            } else { canonical.diagnoses.clone() },
        });
    }
    for defect in &canonical.defects {
        commands.push(RequirementAssessment {
            id: defect.id.clone(),
            source: bindings[0].path.clone(),
            meaning: None,
            check: None,
            satisfied: false,
            cause: Some(VerificationCause::CandidateDefect),
            owner: work_model::CompletionResponsibility::Writer,
            next_action: Some(
                "Repair the established assertion or project-owned harness defect.".into(),
            ),
            bindings: bindings.clone(),
            diagnostics: vec![defect.diagnostic.clone()],
        });
    }
    if !canonical.blockers.is_empty() {
        commands.push(RequirementAssessment {
            id: format!(
                "tester-binding:{}:{}:{}",
                attempt.work_item_id, attempt.id, canonical.config_digest
            ),
            source: bindings[0].path.clone(),
            meaning: None,
            check: None,
            satisfied: false,
            cause: Some(VerificationCause::UninterpretableEvidence),
            owner: work_model::CompletionResponsibility::Tester,
            next_action: Some(
                "Inspect and restore the named immutable proof before retrying verification."
                    .into(),
            ),
            bindings: bindings.clone(),
            diagnostics: canonical.blockers.clone(),
        });
    }
    for reference in &mut references {
        if let Some(ReferenceMeaning::Command {
            command,
            command_index,
            ..
        }) = &reference.meaning
        {
            let matches = commands.iter().filter(|row| matches!(&row.meaning, Some(ReferenceMeaning::Command { command: actual, command_index: actual_index, .. }) if actual == command && command_index.is_none_or(|index| Some(index) == *actual_index))).collect::<Vec<_>>();
            if let [row] = matches.as_slice() {
                reference.check = row.check.clone();
                for binding in &row.bindings {
                    if !reference.bindings.contains(binding) {
                        reference.bindings.push(binding.clone());
                    }
                }
                if reference.satisfied {
                    reference.owner = row.owner;
                }
            }
        }
    }
    Ok(VerificationAssessment {
        candidate_commit: output.commit.clone(),
        references,
        commands,
    })
}

pub(crate) fn snapshot_path(work: &str, attempt: &str, digest: &str) -> Result<String> {
    let hex = digest
        .strip_prefix("sha256:")
        .context("invalid evidence digest")?;
    Ok(format!(
        ".fluent/work/artifacts/{work}/{attempt}/host-evidence/{hex}.json"
    ))
}

pub const VERIFICATION_GATE_SCHEMA_VERSION: u32 = 5;

/// Adapt the one supported legacy gate shape without depending on gate execution.
pub(crate) fn requested_legacy_host_command(gate: &serde_json::Value) -> Option<String> {
    let version = gate.get("schema_version")?.as_u64()?;
    if version == 0
        || version > u64::from(VERIFICATION_GATE_SCHEMA_VERSION)
        || gate.get("phase")?.as_str()? != "final-tester"
        || gate.get("disposition")?.as_str()? != "rejected"
    {
        return None;
    }
    let blocking = gate
        .get("checks")?
        .as_array()?
        .iter()
        .filter(|check| {
            matches!(
                check.get("status").and_then(|status| status.as_str()),
                Some("failed" | "error")
            )
        })
        .collect::<Vec<_>>();
    let [check] = blocking.as_slice() else {
        return None;
    };
    if check.get("name")?.as_str()? != "tester-reference-evidence"
        || check.get("status")?.as_str()? != "failed"
    {
        return None;
    }
    let mut commands = check
        .get("diagnostics")?
        .as_array()?
        .iter()
        .filter_map(|diagnostic| {
            let reference = references::parse_test_reference(
                diagnostic
                    .as_str()?
                    .strip_prefix("no passing Tester result matches ")?,
            )
            .ok()?;
            // Explicit commands name canonical occurrences; this adapter only
            // preserves legacy commands checked outside the configured Tester.
            if reference.declared.is_some() {
                return None;
            }
            reference.command
        })
        .collect::<Vec<_>>();
    commands.sort();
    commands.dedup();
    let [command] = commands.as_slice() else {
        return None;
    };
    Some(command.clone())
}

pub(crate) struct AcceptedCommandProof {
    pub command: String,
    pub(crate) bindings: Vec<work_model::BoundRecheckArtifact>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyGateBinding {
    schema_version: u32,
    candidate_commit: String,
    gate_artifact: work_model::BoundRecheckArtifact,
    gate_evidence: serde_json::Value,
    command: String,
    working_directory: String,
}

pub(crate) fn accepted_legacy_gate_commands(
    project_root: &Path,
    attempt: &work_model::Attempt,
) -> Vec<AcceptedCommandProof> {
    let candidate = attempt
        .tasks
        .iter()
        .rev()
        .find(|task| task.kind == TaskKind::Write && task.status == TaskStatus::Complete)
        .and_then(|task| task.output.as_ref())
        .map(|output| output.commit.as_str());
    attempt
        .evidence_recoveries
        .iter()
        .filter_map(|recovery| {
            if recovery.state != EvidenceRecoveryState::Complete
                || Some(recovery.candidate_commit.as_str()) != candidate
                || recovery.required_check.is_some()
            {
                return None;
            }
            let [target] = recovery.targets.as_slice() else {
                return None;
            };
            if target.role != "tests" {
                return None;
            }
            let (source, bytes) =
                bound_recovery_artifact(project_root, &target.prior_review_artifact).ok()?;
            let binding: LegacyGateBinding = serde_json::from_slice(&bytes).ok()?;
            if binding.schema_version != 1
                || binding.candidate_commit != recovery.candidate_commit
                || snapshot_path(&attempt.work_item_id, &attempt.id, &source.digest).ok()?
                    != source.path
                || binding.gate_evidence.get("candidate_commit")?.as_str()?
                    != recovery.candidate_commit
                || requested_legacy_host_command(&binding.gate_evidence).as_deref()
                    != Some(binding.command.as_str())
                || binding.working_directory != "."
            {
                return None;
            }
            let (attachment, bytes) =
                bound_recovery_artifact(project_root, &recovery.attachment.snapshot_path).ok()?;
            if attachment.digest != recovery.attachment.digest {
                return None;
            }
            let evidence = parse_evidence_document(&bytes).ok()?;
            if evidence.result != EvidenceResult::Pass
                || evidence.check != binding.command
                || evidence.working_directory != binding.working_directory
            {
                return None;
            }
            let review = attempt.tasks.iter().find(|task| {
                Some(&task.id) == target.review_task_id.as_ref()
                    && task.kind == TaskKind::Review
                    && task.role == "tests"
                    && task.status == TaskStatus::Complete
                    && task
                        .evidence_review_context
                        .as_ref()
                        .is_some_and(|context| {
                            context.recovery_id == recovery.id
                                && context.candidate_commit == recovery.candidate_commit
                                && context.attachment == recovery.attachment
                                && context.prior_review_artifact == target.prior_review_artifact
                        })
            })?;
            let (review_source, bytes) =
                bound_recovery_artifact(project_root, &review_artifact_path(review)?).ok()?;
            if review::extract_verdict(std::str::from_utf8(&bytes).ok()?) != review::Verdict::Pass {
                return None;
            }
            Some(AcceptedCommandProof {
                command: binding.command,
                bindings: vec![source, binding.gate_artifact, attachment, review_source],
            })
        })
        .collect()
}

pub(crate) fn accepted_legacy_commands(
    project_root: &Path,
    attempt: &work_model::Attempt,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<AcceptedCommandProof> {
    let mut proofs = accepted_legacy_gate_commands(project_root, attempt);
    proofs.extend(
        accepted_review_check_results(project_root, attempt, now)
            .into_iter()
            .map(|result| AcceptedCommandProof {
                command: result.command,
                bindings: result.bindings,
            }),
    );
    proofs
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FailedCommandRecheckEvidenceBinding {
    pub(crate) schema_version: u32,
    pub(crate) candidate_commit: String,
    pub(crate) recheck_task_id: String,
    pub(crate) source_results: work_model::BoundRecheckArtifact,
    pub(crate) recheck_artifacts: Vec<work_model::BoundRecheckArtifact>,
    pub(crate) command_index: usize,
    pub(crate) command: String,
    /// The command the authorized host actually ran. Older bindings used the
    /// recheck command for both fields, so absence retains that interpretation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) evidence_check: Option<String>,
    pub(crate) working_directory: String,
}

/// Binds a second attachment (with native observations) to the accepted canonical
/// command occurrence it follows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub(crate) struct NativeObservationRecoveryBinding {
    pub(crate) schema_version: u32,
    pub(crate) candidate_commit: String,
    pub(crate) source_recovery_id: String,
    pub(crate) required_check_id: String,
    pub(crate) command_index: usize,
    pub(crate) command: String,
    pub(crate) test_harness: String,
    pub(crate) working_directory: String,
    pub(crate) source_results: work_model::BoundRecheckArtifact,
}

pub(crate) const NATIVE_OBSERVATION_BINDING_SCHEMA_VERSION: u32 = 1;

/// Collect passing test observations from second-attachment native obs recoveries.
///
/// Only observations from a complete recovery with a passing targeted Tests Review
/// are returned. An observation is included only when its harness matches the
/// binding's declared test_harness.
pub(crate) fn accepted_native_observation_tests(
    project_root: &Path,
    attempt: &work_model::Attempt,
    candidate_commit: &str,
    canonical: &CanonicalCheckAssessment,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<crate::tester::TestResult> {
    attempt
        .evidence_recoveries
        .iter()
        .filter_map(|recovery| {
            if recovery.state != EvidenceRecoveryState::Complete
                || recovery.candidate_commit != candidate_commit
            {
                return None;
            }
            let [target] = recovery.targets.as_slice() else {
                return None;
            };
            if target.role != "tests" {
                return None;
            }
            let (_, binding_bytes) =
                bound_recovery_artifact(project_root, &target.prior_review_artifact).ok()?;
            let binding: NativeObservationRecoveryBinding =
                serde_json::from_slice(&binding_bytes).ok()?;
            if binding.schema_version != NATIVE_OBSERVATION_BINDING_SCHEMA_VERSION
                || binding.candidate_commit != candidate_commit
            {
                return None;
            }
            let check = canonical.checks.iter().find(|check| {
                check.id == binding.required_check_id
                    && check.candidate_commit == candidate_commit
                    && check.command == binding.command
                    && check.working_directory == binding.working_directory
            })?;
            let work_model::RequiredCheckSource::CanonicalTester {
                command_index,
                results: source_results,
                ..
            } = &check.source
            else {
                return None;
            };
            if *command_index != binding.command_index
                || *source_results != binding.source_results
                || canonical.outstanding.contains(command_index)
                || validate_required_check_acceptance_at(project_root, attempt, check, now).is_err()
            {
                return None;
            }
            let accepted_source = attempt.evidence_recoveries.iter().rev().find(|source| {
                source
                    .required_check
                    .as_ref()
                    .is_some_and(|bound| bound.id == check.id)
            })?;
            if accepted_source.id != binding.source_recovery_id {
                return None;
            }
            let (bound_results, source_bytes) =
                bound_recovery_artifact(project_root, &source_results.path).ok()?;
            if bound_results.digest != source_results.digest {
                return None;
            }
            let source: crate::tester::TesterResults =
                serde_json::from_slice(&source_bytes).ok()?;
            if source.commands.get(*command_index)?.test_harness != binding.test_harness {
                return None;
            }
            let (att, evidence_bytes) =
                bound_recovery_artifact(project_root, &recovery.attachment.snapshot_path).ok()?;
            if att.digest != recovery.attachment.digest {
                return None;
            }
            let evidence = parse_evidence_document(&evidence_bytes).ok()?;
            if evidence.result != EvidenceResult::Pass
                || evidence.check != binding.command
                || evidence.working_directory != binding.working_directory
                || evidence.native_observations.is_empty()
                || evidence
                    .native_observations
                    .iter()
                    .any(|obs| obs.harness != binding.test_harness)
                || validate_check_applicability_at(&check.applicability, &evidence, now).is_err()
            {
                return None;
            }
            let review_task = target.review_task_id.as_ref().and_then(|id| {
                attempt.tasks.iter().find(|task| {
                    task.id == *id
                        && task.kind == TaskKind::Review
                        && task.role == "tests"
                        && task.status == TaskStatus::Complete
                        && task
                            .evidence_review_context
                            .as_ref()
                            .is_some_and(|context| {
                                context.recovery_id == recovery.id
                                    && context.candidate_commit == recovery.candidate_commit
                                    && context.attachment == recovery.attachment
                                    && context.prior_review_artifact == target.prior_review_artifact
                            })
                })
            })?;
            let review_path = review_artifact_path(review_task)?;
            let (review_art, review_bytes) =
                bound_recovery_artifact(project_root, &review_path).ok()?;
            if target.review_digest.as_deref() != Some(review_art.digest.as_str()) {
                return None;
            }
            if review::extract_verdict(std::str::from_utf8(&review_bytes).ok()?)
                != review::Verdict::Pass
            {
                return None;
            }
            let observations = evidence
                .native_observations
                .iter()
                .filter(|obs| obs.harness == binding.test_harness)
                .map(|obs| crate::tester::TestResult {
                    identity: None,
                    id: obs.id.clone(),
                    test_harness: obs.harness.clone(),
                    status: "pass".into(),
                    duration_ms: None,
                    failure_excerpt: None,
                })
                .collect::<Vec<_>>();
            if observations.is_empty() {
                return None;
            }
            Some(observations)
        })
        .flatten()
        .collect()
}

struct AcceptedRecheckResult {
    source_results: work_model::BoundRecheckArtifact,
    command_index: usize,
    command: String,
    comparison_commit: Option<String>,
    bindings: Vec<work_model::BoundRecheckArtifact>,
}

fn accepted_recheck_result(
    project_root: &Path,
    attempt: &work_model::Attempt,
    recovery: &EvidenceRecovery,
) -> Option<AcceptedRecheckResult> {
    if recovery.state != EvidenceRecoveryState::Complete
        || recovery.targets.len() != 1
        || recovery.targets[0].role != "tests"
    {
        return None;
    }
    let target = &recovery.targets[0];
    let binding_bytes = bound_recovery_artifact(project_root, &target.prior_review_artifact)
        .ok()?
        .1;
    let binding: FailedCommandRecheckEvidenceBinding =
        serde_json::from_slice(&binding_bytes).ok()?;
    let binding_digest = format!("sha256:{:x}", Sha256::digest(&binding_bytes));
    if binding.schema_version != 1
        || binding.candidate_commit != recovery.candidate_commit
        || snapshot_path(
            attempt.work_item_id.as_str(),
            attempt.id.as_str(),
            &binding_digest,
        )
        .ok()
        .as_deref()
            != Some(target.prior_review_artifact.as_str())
        || attempt
            .tasks
            .iter()
            .rev()
            .find(|task| task.kind == TaskKind::Write && task.status == TaskStatus::Complete)
            .and_then(|task| task.output.as_ref())
            .map(|output| output.commit.as_str())
            != Some(recovery.candidate_commit.as_str())
    {
        return None;
    }
    let evidence_bytes = bound_recovery_artifact(project_root, &recovery.attachment.snapshot_path)
        .ok()?
        .1;
    if format!("sha256:{:x}", Sha256::digest(&evidence_bytes)) != recovery.attachment.digest {
        return None;
    }
    let evidence = parse_evidence_document(&evidence_bytes).ok()?;
    let evidence_check = binding
        .evidence_check
        .as_deref()
        .unwrap_or(binding.command.as_str());
    if evidence.result != EvidenceResult::Pass
        || evidence.check != evidence_check
        || evidence_check != binding.command
        || evidence.working_directory != binding.working_directory
    {
        return None;
    }
    let review_task = target.review_task_id.as_ref().and_then(|id| {
        attempt.tasks.iter().find(|task| {
            task.id == *id
                && task.kind == TaskKind::Review
                && task.role == "tests"
                && task.status == TaskStatus::Complete
                && task
                    .evidence_review_context
                    .as_ref()
                    .is_some_and(|context| {
                        context.recovery_id == recovery.id
                            && context.candidate_commit == recovery.candidate_commit
                            && context.prior_review_artifact == target.prior_review_artifact
                            && context.attachment == recovery.attachment
                    })
        })
    })?;
    let review_path = review_artifact_path(review_task)?;
    let (review_binding, review_bytes) =
        bound_recovery_artifact(project_root, &review_path).ok()?;
    if review::extract_verdict(std::str::from_utf8(&review_bytes).ok()?) != review::Verdict::Pass
        || target
            .review_digest
            .as_ref()
            .is_some_and(|digest| digest != &review_binding.digest)
    {
        return None;
    }
    let recheck = attempt.tasks.iter().find(|task| {
        task.id == binding.recheck_task_id
            && task.kind == TaskKind::Tester
            && task.status == TaskStatus::Complete
            && task.is_failed_command_recheck()
    })?;
    let current_artifacts =
        failed_command_recheck_correction_artifacts(project_root, recheck).ok()?;
    if current_artifacts.len() != binding.recheck_artifacts.len()
        || !current_artifacts
            .iter()
            .zip(&binding.recheck_artifacts)
            .all(|(current, bound)| {
                current.path == bound.path
                    && bound_recovery_artifact(project_root, &current.path).is_ok_and(
                        |(_, bytes)| format!("sha256:{:x}", Sha256::digest(&bytes)) == bound.digest,
                    )
            })
    {
        return None;
    }
    let source_bytes = bound_recovery_artifact(project_root, &binding.source_results.path)
        .ok()?
        .1;
    if format!("sha256:{:x}", Sha256::digest(&source_bytes)) != binding.source_results.digest {
        return None;
    }
    let source: crate::tester::TesterResults = serde_json::from_slice(&source_bytes).ok()?;
    let command = source.commands.get(binding.command_index)?;
    let area = &recheck.artifact_area.as_ref()?.path;
    let (_, manifest_bytes) =
        bound_recovery_artifact(project_root, &format!("{area}/failed-command-recheck.json"))
            .ok()?;
    let manifest: crate::tester::FailedCommandRecheckManifest =
        serde_json::from_slice(&manifest_bytes).ok()?;
    let (_, recheck_bytes) =
        bound_recovery_artifact(project_root, &format!("{area}/tester-results.json")).ok()?;
    let recheck_results: crate::tester::TesterResults =
        serde_json::from_slice(&recheck_bytes).ok()?;
    let position = manifest
        .failed_command_indexes
        .iter()
        .position(|index| *index == binding.command_index)?;
    let checked = recheck_results.commands.get(position)?;
    if manifest.source_results != binding.source_results.path
        || manifest.candidate_commit != binding.candidate_commit
        || binding.working_directory != "."
        || checked.command != command.command
        || checked.test_harness != command.test_harness
    {
        return None;
    }

    if source.candidate_commit.as_deref() != Some(recovery.candidate_commit.as_str())
        || source.error.is_some()
        || command.exit_code == 0
        || command
            .process_settlement
            .as_ref()
            .is_some_and(|settlement| !settlement.leaks.is_empty())
        || command.command != binding.command
        || source.summary.fail
            != source
                .tests
                .iter()
                .filter(|test| test.status == "fail")
                .count()
    {
        return None;
    }
    let mut bindings = binding.recheck_artifacts.clone();
    bindings.extend([
        binding.source_results.clone(),
        review_binding,
        work_model::BoundRecheckArtifact {
            path: target.prior_review_artifact.clone(),
            digest: binding_digest,
        },
        work_model::BoundRecheckArtifact {
            path: recovery.attachment.snapshot_path.clone(),
            digest: recovery.attachment.digest.clone(),
        },
    ]);
    Some(AcceptedRecheckResult {
        source_results: binding.source_results,
        command_index: binding.command_index,
        command: binding.command,
        comparison_commit: recheck.review_context.as_ref()?.base_commit.clone(),
        bindings,
    })
}

pub(crate) fn failed_command_recheck_correction_artifacts(
    project_root: &Path,
    recheck: &Task,
) -> Result<Vec<work_model::ArtifactRef>> {
    let area = recheck
        .artifact_area
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("failed-command recheck has no artifact area"))?;
    let manifest_path = validate_managed_file_path(
        project_root,
        &format!("{}/failed-command-recheck.json", area.path),
    )?;
    let manifest: crate::tester::FailedCommandRecheckManifest =
        serde_json::from_slice(&fs::read(&manifest_path)?)?;
    let results_path =
        validate_managed_file_path(project_root, &format!("{}/tester-results.json", area.path))?;
    let results: crate::tester::TesterResults = serde_json::from_slice(&fs::read(&results_path)?)?;
    let expected_commit = recheck
        .review_context
        .as_ref()
        .map(|context| context.candidate_commit.as_str())
        .ok_or_else(|| anyhow::anyhow!("failed-command recheck has no candidate context"))?;
    if results.candidate_commit.as_deref() != Some(expected_commit) {
        bail!(
            "failed-command recheck result is not bound to candidate {}",
            expected_commit
        );
    }
    let indexes_are_strict = manifest
        .failed_command_indexes
        .windows(2)
        .all(|indexes| indexes[0] < indexes[1]);
    let expected_source = recheck
        .input_artifacts
        .first()
        .map(|artifact| artifact.path.as_str());
    if manifest.schema_version != crate::tester::FAILED_COMMAND_RECHECK_SCHEMA_VERSION
        || manifest.candidate_commit != expected_commit
        || manifest.failed_command_indexes.len() != results.commands.len()
        || manifest.failed_command_indexes.is_empty()
        || !indexes_are_strict
        || expected_source != Some(manifest.source_results.as_str())
    {
        bail!("failed-command recheck manifest does not match its candidate and command results");
    }
    let structured_failure = results.summary.fail > 0 || results.error.is_some();
    let failed = results.commands.iter().enumerate().filter(|(_, command)| {
        structured_failure
            || command.exit_code != 0
            || command
                .process_settlement
                .as_ref()
                .is_some_and(|evidence| !evidence.leaks.is_empty())
    });
    let mut failed = failed.peekable();
    if !structured_failure && failed.peek().is_none() {
        return Ok(Vec::new());
    }

    let mut artifacts = vec![
        work_model::ArtifactRef {
            producer_id: recheck.id.clone(),
            path: format!("{}/failed-command-recheck.json", area.path),
        },
        work_model::ArtifactRef {
            producer_id: recheck.id.clone(),
            path: format!("{}/tester-results.json", area.path),
        },
    ];
    for (index, command) in failed {
        for (actual, expected) in [
            (&command.stdout_log, format!("commands/{index}-stdout.log")),
            (&command.stderr_log, format!("commands/{index}-stderr.log")),
        ] {
            if actual != &expected
                || validate_managed_file_path(project_root, &format!("{}/{}", area.path, expected))
                    .is_err()
            {
                bail!("failed-command recheck log path {actual:?} does not match {expected:?}");
            }
            artifacts.push(work_model::ArtifactRef {
                producer_id: recheck.id.clone(),
                path: format!("{}/{}", area.path, expected),
            });
        }
    }
    Ok(artifacts)
}

#[cfg(test)]
mod native_observation_tests {
    use super::*;
    use crate::{
        host_evidence::{
            EvidenceDocument, EvidenceResult, attach_native_observations_for_required_check,
        },
        recovery_contracts::{
            assess_canonical_checks,
            verification_tests::{accept_first_check, unique_harness_fixture},
        },
        work_model::{
            EvidenceRecoveryState, NativeTestObservation, TaskKind, TaskStatus, WorkModelStore,
        },
    };
    use std::fs;

    /// Build a complete native observation recovery: accepted command-level proof,
    /// then a native observation attachment with a passed targeted Tests Review.
    fn make_accepted_native_recovery(
        root: &std::path::Path,
        item: &mut work_model::WorkItem,
    ) -> (String, String, String) {
        fs::write(
            root.join(".git/info/exclude"),
            ".fluent/work/\nnative-obs-evidence.json\n",
        )
        .unwrap();
        let candidate = item.attempts[0]
            .tasks
            .iter()
            .rev()
            .find(|t| t.kind == TaskKind::Write && t.status == TaskStatus::Complete)
            .and_then(|t| t.output.as_ref())
            .unwrap()
            .commit
            .clone();
        item.attempts[0].status = work_model::AttemptStatus::Complete;
        let assessment = assess_canonical_checks(root, &item.attempts[0]).unwrap();
        let check_id = assessment.checks[0].id.clone();
        let command = assessment.checks[0].command.clone();

        let store = WorkModelStore::new(root);
        store.create_work_item(item).unwrap();

        let obs_document = EvidenceDocument {
            environment: Default::default(),
            comparison_commits: Default::default(),
            schema_version: 1,
            producer: "authorized host".to_string(),
            check: command.clone(),
            working_directory: ".".to_string(),
            result: EvidenceResult::Pass,
            run_at: work_model::now_iso8601(),
            output: "passed with observations".to_string(),
            native_observations: vec![NativeTestObservation {
                id: "tests/native.sh::observed-test".to_string(),
                harness: "shell-harness".to_string(),
            }],
        };
        let evidence_path = root.join("native-obs-evidence.json");
        fs::write(
            &evidence_path,
            serde_json::to_vec_pretty(&obs_document).unwrap(),
        )
        .unwrap();

        let recovery = attach_native_observations_for_required_check(
            root,
            &store,
            &item.id,
            "attempt-1",
            &candidate,
            &check_id,
            &evidence_path,
        )
        .unwrap();

        // Complete the targeted Tests Review so the recovery settles.
        store
            .mutate_work_item(&item.id, |item| {
                let attempt = &mut item.attempts[0];
                let review_id = recovery.targets[0].review_task_id.as_ref().unwrap().clone();
                let review = attempt
                    .tasks
                    .iter_mut()
                    .find(|t| t.id == review_id)
                    .unwrap();
                work_model::set_task_terminal(review, TaskStatus::Complete);
                let area = root.join(&review.artifact_area.as_ref().unwrap().path);
                fs::create_dir_all(&area).unwrap();
                fs::write(area.join("review.md"), "Verdict: pass\n").unwrap();
                let nat_recovery = attempt
                    .evidence_recoveries
                    .iter_mut()
                    .find(|r| r.id == recovery.id)
                    .unwrap();
                nat_recovery.state = EvidenceRecoveryState::Complete;
                nat_recovery.targets[0].review_digest =
                    Some(crate::verification_policy::digest(b"Verdict: pass\n"));
                Ok(())
            })
            .unwrap();

        (item.id.clone(), candidate, recovery.id)
    }

    #[test]
    fn reviewed_host_native_results_satisfy_matching_references_only() {
        let (root, mut item) = unique_harness_fixture();
        accept_first_check(root.path(), &mut item);
        item.attempts[0]
            .writer_completion_contract
            .as_mut()
            .unwrap()
            .requirements[0]
            .verification_refs = vec![
            r#"{"kind":"named-test","path":"tests/native.sh","name":"observed-test"}"#.into(),
            r#"{"kind":"named-test","path":"tests/native.sh","name":"unobserved-test"}"#.into(),
        ];

        let (_, _, _recovery_id) = make_accepted_native_recovery(root.path(), &mut item);

        let item = WorkModelStore::new(root.path())
            .read_work_item("verification")
            .unwrap();
        let attempt = &item.attempts[0];

        let assessment = assess_current_verification(root.path(), attempt).unwrap();

        // The named-test reference for the observed test must be satisfied.
        assert!(
            assessment.references.iter().any(|r| r.satisfied),
            "reviewed native observation must satisfy its matching reference: {:?}",
            assessment.references
        );

        assert!(
            assessment.references.iter().any(|row| !row.satisfied),
            "unobserved named references must stay unresolved"
        );

        // The canonical Tester results file must be unmodified.
        let canonical = assess_canonical_checks(root.path(), attempt).unwrap();
        let original_bytes = fs::read(&canonical.results_path).unwrap();
        let results: crate::tester::TesterResults =
            serde_json::from_slice(&original_bytes).unwrap();
        assert!(
            !results
                .tests
                .iter()
                .any(|t| t.id == "tests/native.sh::observed-test"),
            "canonical Tester results must not be modified by native observation attachment"
        );
    }

    #[test]
    fn native_observations_reject_changed_command_source_binding() {
        let (root, mut item) = unique_harness_fixture();
        accept_first_check(root.path(), &mut item);
        item.attempts[0]
            .writer_completion_contract
            .as_mut()
            .unwrap()
            .requirements[0]
            .verification_refs =
            vec![r#"{"kind":"named-test","path":"tests/native.sh","name":"observed-test"}"#.into()];
        let (_, _, recovery_id) = make_accepted_native_recovery(root.path(), &mut item);
        let store = WorkModelStore::new(root.path());
        let stored = store.read_work_item("verification").unwrap();
        let recovery = stored.attempts[0]
            .evidence_recoveries
            .iter()
            .find(|recovery| recovery.id == recovery_id)
            .unwrap();
        let binding_path = root.path().join(&recovery.targets[0].prior_review_artifact);
        let mut binding: NativeObservationRecoveryBinding =
            serde_json::from_slice(&fs::read(&binding_path).unwrap()).unwrap();
        binding.source_recovery_id = "different-command-proof".into();
        fs::write(&binding_path, serde_json::to_vec(&binding).unwrap()).unwrap();

        let assessment = assess_current_verification(root.path(), &stored.attempts[0]).unwrap();
        assert!(assessment.references.iter().all(|row| !row.satisfied));
    }

    #[test]
    fn native_observations_reject_dirty_candidate_attachment() {
        let (root, mut item) = unique_harness_fixture();
        accept_first_check(root.path(), &mut item);
        let (item_id, candidate, _) = make_accepted_native_recovery(root.path(), &mut item);
        let store = WorkModelStore::new(root.path());
        let stored = store.read_work_item(&item_id).unwrap();
        let check = assess_canonical_checks(root.path(), &stored.attempts[0])
            .unwrap()
            .checks[0]
            .id
            .clone();
        fs::write(
            root.path().join("tests/native.sh"),
            "changed candidate source\n",
        )
        .unwrap();
        let error = attach_native_observations_for_required_check(
            root.path(),
            &store,
            &item_id,
            "attempt-1",
            &candidate,
            &check,
            &root.path().join("native-obs-evidence.json"),
        )
        .expect_err("dirty candidate must not authorize an attachment or its replay");
        assert!(format!("{error:#}").contains("unchanged clean candidate"));
    }

    #[test]
    fn native_observations_reject_unsettled_execution() {
        let (root, mut item) = unique_harness_fixture();
        accept_first_check(root.path(), &mut item);
        let (item_id, candidate, _) = make_accepted_native_recovery(root.path(), &mut item);
        let store = WorkModelStore::new(root.path());
        let stored = store.read_work_item(&item_id).unwrap();
        let check = assess_canonical_checks(root.path(), &stored.attempts[0])
            .unwrap()
            .checks[0]
            .id
            .clone();
        store
            .mutate_work_item(&item_id, |item| {
                item.attempts[0].tasks.last_mut().unwrap().status = TaskStatus::Executing;
                Ok(())
            })
            .unwrap();
        let error = attach_native_observations_for_required_check(
            root.path(),
            &store,
            &item_id,
            "attempt-1",
            &candidate,
            &check,
            &root.path().join("native-obs-evidence.json"),
        )
        .expect_err("live execution must block an attachment or its replay");
        assert!(format!("{error:#}").contains("owners are not settled"));
    }

    #[test]
    fn native_observation_documents_reject_malformed_and_duplicate_entries() {
        let base = serde_json::json!({
            "schema_version": 1, "producer": "host", "check": "false",
            "working_directory": ".", "result": "pass",
            "run_at": "2026-10-18T00:00:00Z", "output": "ok"
        });
        for observations in [
            serde_json::json!([{"id":" ","harness":"cargo-test"}]),
            serde_json::json!([{"id":"case","harness":""}]),
            serde_json::json!([{"id":"case","harness":"cargo-test","status":"fail"}]),
            serde_json::json!([{"id":"case","harness":"cargo-test"},{"id":"case","harness":"cargo-test"}]),
            serde_json::json!(
                (0..=MAX_NATIVE_OBSERVATIONS)
                    .map(|i| serde_json::json!({"id":format!("case-{i}"),"harness":"cargo-test"}))
                    .collect::<Vec<_>>()
            ),
        ] {
            let mut document = base.clone();
            document["native_observations"] = observations;
            assert!(parse_evidence_document(&serde_json::to_vec(&document).unwrap()).is_err());
        }
    }

    #[test]
    fn command_level_host_pass_does_not_invent_named_results() {
        let (root, mut item) = unique_harness_fixture();
        accept_first_check(root.path(), &mut item);
        item.attempts[0]
            .writer_completion_contract
            .as_mut()
            .unwrap()
            .requirements[0]
            .verification_refs =
            vec![r#"{"kind":"named-test","path":"tests/native.sh","name":"any-test-id"}"#.into()];
        item.attempts[0].status = work_model::AttemptStatus::Complete;

        let store = WorkModelStore::new(root.path());
        store.create_work_item(&item).unwrap();

        let item = store.read_work_item("verification").unwrap();
        let attempt = &item.attempts[0];
        let assessment = assess_current_verification(root.path(), attempt).unwrap();

        // A command-level host pass alone must not satisfy a named-test reference.
        assert!(
            assessment.references.iter().all(|r| !r.satisfied),
            "command-level pass must not invent named test results: {:?}",
            assessment.references
        );
    }
}
