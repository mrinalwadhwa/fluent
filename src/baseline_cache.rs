//! Cache for immutable pre-write Tester baselines.
//!
//! A successful land publishes one durable baseline for its exact Tester
//! boundary. Direct and scheduler-launched Attempts prefer that evidence.
//! Scheduler children retain a temporary session cache as a fallback for source
//! commits that have no landed baseline.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::content::ContentResolver;
use crate::tester::{TesterOutcome, TesterResults};

pub(crate) const BASELINE_SESSION_ENV: &str = "FLUENT_SCHEDULER_BASELINE_SESSION";
const CACHE_ROOT: &str = ".fluent/work/baselines";
const LANDED_ROOT: &str = "landed";
const SESSIONS_ROOT: &str = "sessions";
const PROVENANCE_FILE: &str = "baseline-provenance.json";
const MANIFEST_FILE: &str = "manifest.json";
const FINAL_TESTER_SEED_FILE: &str = "landed-baseline-seed.json";
const RESULTS_FILE: &str = "tester-results.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct BaselineIdentity {
    schema_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scheduler_session: Option<String>,
    source_commit: String,
    tester_config_digest: String,
    extractor_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    permission_policy_digest: Option<String>,
    fluent_version: String,
    operating_system: String,
    architecture: String,
    sandboxed: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct BaselineManifest {
    identity: BaselineIdentity,
    cache_key: String,
    artifact_digest: String,
    results_digest: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct BaselineProvenance {
    schema_version: u32,
    cache_key: String,
    source: String,
    source_commit: String,
    artifact_digest: String,
    results_digest: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct FinalTesterSeed {
    identity: BaselineIdentity,
    artifact_digest: String,
    results_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CaptureDisposition {
    AttemptLocal,
    Published { cache_key: String },
    Reused { cache_key: String },
    ReusedLanded { cache_key: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LandedSeedDisposition {
    Promoted { cache_key: String },
    Captured { cache_key: String },
    Reused { cache_key: String },
}

/// Start one elected scheduler's cache, removing evidence left by an interrupted
/// predecessor before any child can publish a new baseline.
pub(crate) fn prepare_scheduler_session(project_root: &Path, session: &str) -> Result<()> {
    let root = project_root.join(CACHE_ROOT).join(SESSIONS_ROOT);
    if root.exists() {
        let metadata = fs::symlink_metadata(&root)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            bail!("shared baseline root is not a regular directory");
        }
        fs::remove_dir_all(&root)
            .with_context(|| format!("clear stale shared baselines at {}", root.display()))?;
    }
    fs::create_dir_all(root.join(session_key(session)))?;
    Ok(())
}

/// Remove the elected scheduler's temporary cache after its workers have
/// stopped. Each completed Attempt keeps its own evidence copy.
pub(crate) fn finish_scheduler_session(project_root: &Path, session: &str) -> Result<()> {
    let session_dir = project_root
        .join(CACHE_ROOT)
        .join(SESSIONS_ROOT)
        .join(session_key(session));
    match fs::remove_dir_all(&session_dir) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| {
            format!(
                "remove shared baseline scheduler session {}",
                session_dir.display()
            )
        }),
    }
}

pub(crate) fn capture(
    project_root: &Path,
    candidate_workspace: &Path,
    attempt_artifact_dir: &Path,
    no_sandbox: bool,
    resolver: &ContentResolver,
) -> Result<CaptureDisposition> {
    let landed_identity = resolve_identity(candidate_workspace, no_sandbox, None)?;
    if let Some((manifest, cache_key)) = reuse_existing(project_root, &landed_identity)? {
        let landed_cache_dir =
            identity_cache_root(project_root, &landed_identity)?.join(&cache_key);
        copy_tree(&landed_cache_dir.join("artifact"), attempt_artifact_dir)?;
        write_provenance(attempt_artifact_dir, &manifest, "reused-landed-baseline")?;
        return Ok(CaptureDisposition::ReusedLanded { cache_key });
    }

    let Some(session) = std::env::var(BASELINE_SESSION_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        fs::create_dir_all(attempt_artifact_dir)?;
        crate::tester::run(
            candidate_workspace,
            attempt_artifact_dir,
            no_sandbox,
            resolver,
        )?;
        return Ok(CaptureDisposition::AttemptLocal);
    };

    let identity = resolve_identity(candidate_workspace, no_sandbox, Some(&session))?;
    capture_with(
        project_root,
        attempt_artifact_dir,
        identity,
        |artifact_dir| crate::tester::run(candidate_workspace, artifact_dir, no_sandbox, resolver),
    )
}

fn capture_with(
    project_root: &Path,
    attempt_artifact_dir: &Path,
    identity: BaselineIdentity,
    run: impl FnOnce(&Path) -> Result<TesterOutcome>,
) -> Result<CaptureDisposition> {
    let cache_key = identity_key(&identity)?;
    let cache_root = identity_cache_root(project_root, &identity)?;
    let cache_dir = cache_root.join(&cache_key);
    let lock_path = cache_root.join("locks").join(format!("{cache_key}.lock"));
    let _lease = crate::lease::acquire_blocking(&lock_path)
        .with_context(|| format!("acquire shared baseline lock {}", lock_path.display()))?;

    if let Some(manifest) = validated_manifest(project_root, &cache_dir, &identity, &cache_key)? {
        copy_tree(&cache_dir.join("artifact"), attempt_artifact_dir)?;
        let source = if identity.scheduler_session.is_some() {
            "reused-scheduler-baseline"
        } else {
            "reused-landed-baseline"
        };
        write_provenance(attempt_artifact_dir, &manifest, source)?;
        return Ok(CaptureDisposition::Reused { cache_key });
    }

    fs::create_dir_all(&cache_root)?;
    let staging = tempfile::Builder::new()
        .prefix(".baseline-")
        .tempdir_in(&cache_root)
        .context("create shared baseline staging directory")?;
    let artifact_dir = staging.path().join("artifact");
    fs::create_dir_all(&artifact_dir)?;
    let outcome = run(&artifact_dir);

    match outcome {
        Ok(TesterOutcome::Passed | TesterOutcome::TestFailures) => {}
        Ok(TesterOutcome::HarnessError) => {
            clear_provenance(attempt_artifact_dir)?;
            copy_tree(&artifact_dir, attempt_artifact_dir)?;
            return Ok(CaptureDisposition::AttemptLocal);
        }
        Err(error) => {
            if artifact_dir.exists() {
                clear_provenance(attempt_artifact_dir)?;
                copy_tree(&artifact_dir, attempt_artifact_dir)?;
            }
            return Err(error);
        }
    }

    let results_path = artifact_dir.join(RESULTS_FILE);
    let results_bytes = fs::read(&results_path).with_context(|| {
        format!(
            "shared baseline produced no readable results at {}",
            results_path.display()
        )
    })?;
    let results: TesterResults =
        serde_json::from_slice(&results_bytes).context("parse shared baseline Tester results")?;
    if results.error.is_some() {
        clear_provenance(attempt_artifact_dir)?;
        copy_tree(&artifact_dir, attempt_artifact_dir)?;
        return Ok(CaptureDisposition::AttemptLocal);
    }

    if let Err(error) = validate_permission_proof(project_root, &artifact_dir, &identity, &results)
    {
        clear_provenance(attempt_artifact_dir)?;
        copy_tree(&artifact_dir, attempt_artifact_dir)?;
        return Err(error);
    }

    let manifest = BaselineManifest {
        identity,
        cache_key: cache_key.clone(),
        artifact_digest: digest_tree(&artifact_dir)?,
        results_digest: digest_bytes(&results_bytes),
    };
    crate::atomic_write::atomic_write(
        &staging.path().join(MANIFEST_FILE),
        &serde_json::to_vec_pretty(&manifest)?,
    )?;
    let staged_path = staging.keep();
    fs::rename(&staged_path, &cache_dir).with_context(|| {
        format!(
            "publish shared baseline {} as {}",
            staged_path.display(),
            cache_dir.display()
        )
    })?;
    copy_tree(&cache_dir.join("artifact"), attempt_artifact_dir)?;
    let source = if manifest.identity.scheduler_session.is_some() {
        "captured-scheduler-baseline"
    } else {
        "captured-landed-baseline"
    };
    write_provenance(attempt_artifact_dir, &manifest, source)?;
    Ok(CaptureDisposition::Published { cache_key })
}

fn resolve_identity(
    candidate_workspace: &Path,
    no_sandbox: bool,
    scheduler_session: Option<&str>,
) -> Result<BaselineIdentity> {
    let config =
        crate::tester::read_tester_config(&candidate_workspace.join(".fluent/tester.yaml"))?;
    let requests_permission = config
        .commands
        .iter()
        .any(|command| !command.permissions.is_empty());
    anyhow::ensure!(
        !requests_permission || !no_sandbox,
        "process-table-read requires the macOS sandbox"
    );
    Ok(BaselineIdentity {
        schema_version: if requests_permission { 3 } else { 2 },
        scheduler_session: scheduler_session.map(str::to_string),
        source_commit: crate::git::run_stdout(
            candidate_workspace,
            &["rev-parse", "HEAD"],
            "resolve shared baseline source commit",
        )?,
        tester_config_digest: digest_file(&candidate_workspace.join(".fluent/tester.yaml"))?,
        extractor_digest: digest_file(&candidate_workspace.join(".fluent/extract-tester-results"))?,
        permission_policy_digest: requests_permission
            .then(crate::tester_permissions::permission_policy_digest),
        fluent_version: crate::version::version_tag(),
        operating_system: std::env::consts::OS.to_string(),
        architecture: std::env::consts::ARCH.to_string(),
        sandboxed: !no_sandbox,
    })
}

fn identity_cache_root(project_root: &Path, identity: &BaselineIdentity) -> Result<PathBuf> {
    let root = project_root.join(CACHE_ROOT);
    match identity.scheduler_session.as_deref() {
        Some(session) if !session.trim().is_empty() => {
            Ok(root.join(SESSIONS_ROOT).join(session_key(session)))
        }
        Some(_) => bail!("shared baseline scheduler session must not be empty"),
        None => Ok(root.join(LANDED_ROOT)),
    }
}

fn reuse_existing(
    project_root: &Path,
    identity: &BaselineIdentity,
) -> Result<Option<(BaselineManifest, String)>> {
    let cache_key = identity_key(identity)?;
    let cache_root = identity_cache_root(project_root, identity)?;
    let lock_path = cache_root.join("locks").join(format!("{cache_key}.lock"));
    let _lease = crate::lease::acquire_blocking(&lock_path)
        .with_context(|| format!("acquire shared baseline lock {}", lock_path.display()))?;
    Ok(validated_manifest(
        project_root,
        &cache_root.join(&cache_key),
        identity,
        &cache_key,
    )?
    .map(|manifest| (manifest, cache_key)))
}

/// Bind a completed final Tester artifact to the immutable baseline identity it
/// can seed if that exact commit later lands.
pub(crate) fn record_final_tester_seed(
    candidate_workspace: &Path,
    artifact_dir: &Path,
    no_sandbox: bool,
) -> Result<()> {
    let identity = resolve_identity(candidate_workspace, no_sandbox, None)?;
    let results_bytes = fs::read(artifact_dir.join(RESULTS_FILE))?;
    let results: TesterResults = serde_json::from_slice(&results_bytes)?;
    if results.error.is_some() {
        bail!("final Tester evidence contains a harness error");
    }
    if results.candidate_commit.as_deref() != Some(identity.source_commit.as_str()) {
        bail!(
            "final Tester evidence is not bound to its exact source commit {}",
            identity.source_commit
        );
    }
    validate_permission_proof(candidate_workspace, artifact_dir, &identity, &results)?;
    let seed = FinalTesterSeed {
        identity,
        artifact_digest: digest_tree_without_seed(artifact_dir)?,
        results_digest: digest_bytes(&results_bytes),
    };
    crate::atomic_write::atomic_write(
        &artifact_dir.join(FINAL_TESTER_SEED_FILE),
        &serde_json::to_vec_pretty(&seed)?,
    )?;
    Ok(())
}

/// Promote final Tester evidence when it exactly matches the landed commit and
/// the current land boundary. A mismatch is not an error: the caller captures a
/// fresh landed baseline instead.
pub(crate) fn promote_final_tester(
    project_root: &Path,
    merged_workspace: &Path,
    artifact_dir: &Path,
    merged_commit: &str,
    no_sandbox: bool,
) -> Result<Option<LandedSeedDisposition>> {
    let Some(seed) =
        validated_final_tester_seed(merged_workspace, artifact_dir, merged_commit, no_sandbox)?
    else {
        return Ok(None);
    };
    Ok(Some(publish_existing_artifact(
        project_root,
        artifact_dir,
        seed.identity,
    )?))
}

/// Validate exact final evidence without publishing a baseline before land.
pub(crate) fn reusable_final_tester(
    candidate_workspace: &Path,
    artifact_dir: &Path,
    candidate_commit: &str,
    no_sandbox: bool,
) -> Result<bool> {
    Ok(validated_final_tester_seed(
        candidate_workspace,
        artifact_dir,
        candidate_commit,
        no_sandbox,
    )?
    .is_some())
}

fn validated_final_tester_seed(
    candidate_workspace: &Path,
    artifact_dir: &Path,
    candidate_commit: &str,
    no_sandbox: bool,
) -> Result<Option<FinalTesterSeed>> {
    let seed_path = artifact_dir.join(FINAL_TESTER_SEED_FILE);
    if !seed_path.is_file() {
        return Ok(None);
    }
    let seed: FinalTesterSeed = serde_json::from_slice(&fs::read(&seed_path)?)?;
    let expected = resolve_identity(candidate_workspace, no_sandbox, None)?;
    if expected.source_commit != candidate_commit || seed.identity != expected {
        return Ok(None);
    }

    let results_bytes = fs::read(artifact_dir.join(RESULTS_FILE))?;
    let results: TesterResults = serde_json::from_slice(&results_bytes)?;
    if results.error.is_some()
        || results.candidate_commit.as_deref() != Some(candidate_commit)
        || digest_bytes(&results_bytes) != seed.results_digest
        || digest_tree_without_seed(artifact_dir)? != seed.artifact_digest
    {
        bail!("final Tester seed evidence is not trustworthy");
    }

    validate_permission_proof(candidate_workspace, artifact_dir, &seed.identity, &results)?;
    Ok(Some(seed))
}

fn publish_existing_artifact(
    project_root: &Path,
    artifact_dir: &Path,
    identity: BaselineIdentity,
) -> Result<LandedSeedDisposition> {
    let cache_key = identity_key(&identity)?;
    let cache_root = identity_cache_root(project_root, &identity)?;
    let cache_dir = cache_root.join(&cache_key);
    let lock_path = cache_root.join("locks").join(format!("{cache_key}.lock"));
    let _lease = crate::lease::acquire_blocking(&lock_path)
        .with_context(|| format!("acquire landed baseline lock {}", lock_path.display()))?;

    if validated_manifest(project_root, &cache_dir, &identity, &cache_key)?.is_some() {
        return Ok(LandedSeedDisposition::Reused { cache_key });
    }

    fs::create_dir_all(&cache_root)?;
    let staging = tempfile::Builder::new()
        .prefix(".baseline-")
        .tempdir_in(&cache_root)
        .context("create landed baseline staging directory")?;
    let staged_artifact = staging.path().join("artifact");
    copy_tree(artifact_dir, &staged_artifact)?;
    let results_bytes = fs::read(staged_artifact.join(RESULTS_FILE))?;
    let results: TesterResults = serde_json::from_slice(&results_bytes)?;
    validate_permission_proof(project_root, &staged_artifact, &identity, &results)?;
    let manifest = BaselineManifest {
        identity,
        cache_key: cache_key.clone(),
        artifact_digest: digest_tree(&staged_artifact)?,
        results_digest: digest_bytes(&results_bytes),
    };
    crate::atomic_write::atomic_write(
        &staging.path().join(MANIFEST_FILE),
        &serde_json::to_vec_pretty(&manifest)?,
    )?;
    let staged_path = staging.keep();
    fs::rename(&staged_path, &cache_dir).with_context(|| {
        format!(
            "publish landed baseline {} as {}",
            staged_path.display(),
            cache_dir.display()
        )
    })?;
    Ok(LandedSeedDisposition::Promoted { cache_key })
}

pub(crate) fn capture_landed(
    project_root: &Path,
    merged_workspace: &Path,
    landing_artifact_dir: &Path,
    no_sandbox: bool,
    resolver: &ContentResolver,
) -> Result<LandedSeedDisposition> {
    capture_landed_with(
        project_root,
        merged_workspace,
        landing_artifact_dir,
        no_sandbox,
        |artifact_dir| crate::tester::run(merged_workspace, artifact_dir, no_sandbox, resolver),
    )
}

pub(crate) fn capture_landed_with(
    project_root: &Path,
    merged_workspace: &Path,
    landing_artifact_dir: &Path,
    no_sandbox: bool,
    run: impl FnOnce(&Path) -> Result<TesterOutcome>,
) -> Result<LandedSeedDisposition> {
    let identity = resolve_identity(merged_workspace, no_sandbox, None)?;
    match capture_with(project_root, landing_artifact_dir, identity, run)? {
        CaptureDisposition::Published { cache_key } => {
            Ok(LandedSeedDisposition::Captured { cache_key })
        }
        CaptureDisposition::Reused { cache_key }
        | CaptureDisposition::ReusedLanded { cache_key } => {
            Ok(LandedSeedDisposition::Reused { cache_key })
        }
        CaptureDisposition::AttemptLocal => {
            bail!("landed baseline Tester did not produce reusable evidence")
        }
    }
}

fn identity_key(identity: &BaselineIdentity) -> Result<String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(identity)?)
    ))
}

fn validated_manifest(
    project_root: &Path,
    cache_dir: &Path,
    identity: &BaselineIdentity,
    cache_key: &str,
) -> Result<Option<BaselineManifest>> {
    if !cache_dir.exists() {
        return Ok(None);
    }
    let metadata = fs::symlink_metadata(cache_dir)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("shared baseline cache is not a regular directory");
    }
    let manifest_bytes =
        fs::read(cache_dir.join(MANIFEST_FILE)).context("read shared baseline manifest")?;
    let manifest: BaselineManifest =
        serde_json::from_slice(&manifest_bytes).context("parse shared baseline manifest")?;
    if manifest.identity != *identity
        || manifest.cache_key != cache_key
        || !valid_cache_key(&manifest.cache_key)
    {
        bail!("shared baseline manifest identity does not match its cache key");
    }
    let artifact_dir = cache_dir.join("artifact");
    if digest_tree(&artifact_dir)? != manifest.artifact_digest {
        bail!("shared baseline artifact digest does not match its manifest");
    }
    let results_bytes = fs::read(artifact_dir.join(RESULTS_FILE))?;
    let results: TesterResults = serde_json::from_slice(&results_bytes)?;
    if results.error.is_some() || digest_bytes(&results_bytes) != manifest.results_digest {
        bail!("shared baseline results are not trustworthy");
    }
    validate_permission_proof(project_root, &artifact_dir, identity, &results)?;
    Ok(Some(manifest))
}

fn validate_permission_proof(
    project_root: &Path,
    artifact_dir: &Path,
    identity: &BaselineIdentity,
    results: &TesterResults,
) -> Result<()> {
    if identity.permission_policy_digest.is_none() {
        return Ok(());
    }
    anyhow::ensure!(
        identity.permission_policy_digest.as_deref()
            == Some(crate::tester_permissions::permission_policy_digest().as_str()),
        "baseline permission policy changed"
    );
    let config = crate::git::run_raw(
        project_root,
        &[
            "show",
            &format!("{}:.fluent/tester.yaml", identity.source_commit),
        ],
    )?;
    anyhow::ensure!(
        config.status.success() && digest_bytes(&config.stdout) == identity.tester_config_digest,
        "baseline configuration does not match its source"
    );
    let config: crate::tester::TesterConfig = serde_yaml::from_slice(&config.stdout)?;
    crate::tester_permissions::validate_run_permissions(
        &identity.source_commit,
        &identity.tester_config_digest,
        &identity.extractor_digest,
        &config.commands,
        results,
        |path| Ok(fs::read(artifact_dir.join(path))?),
    )
}

fn write_provenance(
    attempt_artifact_dir: &Path,
    manifest: &BaselineManifest,
    source: &str,
) -> Result<()> {
    fs::create_dir_all(attempt_artifact_dir)?;
    let provenance = BaselineProvenance {
        schema_version: 1,
        cache_key: manifest.cache_key.clone(),
        source: source.to_string(),
        source_commit: manifest.identity.source_commit.clone(),
        artifact_digest: manifest.artifact_digest.clone(),
        results_digest: manifest.results_digest.clone(),
    };
    crate::atomic_write::atomic_write(
        &attempt_artifact_dir.join(PROVENANCE_FILE),
        &serde_json::to_vec_pretty(&provenance)?,
    )?;
    Ok(())
}

fn clear_provenance(attempt_artifact_dir: &Path) -> Result<()> {
    match fs::remove_file(attempt_artifact_dir.join(PROVENANCE_FILE)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("remove stale shared baseline provenance"),
    }
}

fn session_key(session: &str) -> String {
    format!("{:x}", Sha256::digest(session.as_bytes()))
}

fn digest_file(path: &Path) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("read {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

fn digest_bytes(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn digest_tree(root: &Path) -> Result<String> {
    let mut files = Vec::new();
    collect_files(root, root, &mut files)?;
    digest_files(root, files)
}

fn digest_tree_without_seed(root: &Path) -> Result<String> {
    let mut files = Vec::new();
    collect_files(root, root, &mut files)?;
    files.retain(|path| path != Path::new(FINAL_TESTER_SEED_FILE));
    digest_files(root, files)
}

fn digest_files(root: &Path, mut files: Vec<PathBuf>) -> Result<String> {
    files.sort();
    let mut hasher = Sha256::new();
    for relative in files {
        let path = root.join(&relative);
        hasher.update(relative.to_string_lossy().as_bytes());
        hasher.update([0]);
        hasher.update(digest_file(&path)?.as_bytes());
        hasher.update([0]);
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

fn collect_files(root: &Path, path: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        bail!(
            "shared baseline artifact contains a symlink: {}",
            path.display()
        );
    }
    if metadata.is_file() {
        files.push(path.strip_prefix(root)?.to_path_buf());
        return Ok(());
    }
    if !metadata.is_dir() {
        bail!("shared baseline artifact contains an unsupported entry");
    }
    for entry in fs::read_dir(path)? {
        collect_files(root, &entry?.path(), files)?;
    }
    Ok(())
}

fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    if metadata.file_type().is_symlink() {
        bail!(
            "baseline artifact copy refuses symlink {}",
            source.display()
        );
    }
    if metadata.is_dir() {
        fs::create_dir_all(destination)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            copy_tree(&entry.path(), &destination.join(entry.file_name()))?;
        }
    } else if metadata.is_file() {
        fs::copy(source, destination)?;
    } else {
        bail!("baseline artifact copy refuses unsupported filesystem entry");
    }
    Ok(())
}

fn valid_cache_key(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};

    fn initialized_project(path: &Path) -> String {
        fs::create_dir_all(path.join(".fluent")).unwrap();
        fs::write(
            path.join(".fluent/tester.yaml"),
            "commands:\n  - command: echo ok\n    test_harness: shell-harness\n",
        )
        .unwrap();
        fs::write(
            path.join(".fluent/extract-tester-results"),
            "#!/bin/sh\nexit 0\n",
        )
        .unwrap();
        crate::git::run(path, &["init", "-q", "-b", "main"], "init baseline fixture").unwrap();
        crate::git::run(
            path,
            &["config", "user.email", "test@example.com"],
            "configure fixture email",
        )
        .unwrap();
        crate::git::run(
            path,
            &["config", "user.name", "Test User"],
            "configure fixture name",
        )
        .unwrap();
        crate::git::run(path, &["add", ".fluent"], "stage baseline fixture").unwrap();
        crate::git::run(path, &["commit", "-q", "-m", "baseline"], "commit fixture").unwrap();
        crate::git::run_stdout(path, &["rev-parse", "HEAD"], "resolve fixture head").unwrap()
    }

    fn identity(session: &str, commit: &str) -> BaselineIdentity {
        BaselineIdentity {
            schema_version: 2,
            scheduler_session: Some(session.to_string()),
            source_commit: commit.to_string(),
            tester_config_digest: "sha256:config".to_string(),
            extractor_digest: "sha256:extractor".to_string(),
            permission_policy_digest: None,
            fluent_version: "0.1.5 test".to_string(),
            operating_system: "test-os".to_string(),
            architecture: "test-arch".to_string(),
            sandboxed: false,
        }
    }

    fn landed_identity(commit: &str) -> BaselineIdentity {
        BaselineIdentity {
            scheduler_session: None,
            ..identity("unused", commit)
        }
    }

    #[test]
    fn permission_request_adds_policy_to_cache_identity() {
        let root = tempfile::tempdir().unwrap();
        initialized_project(root.path());
        let legacy = resolve_identity(root.path(), false, None).unwrap();
        let legacy_value = serde_json::to_value(&legacy).unwrap();
        assert!(legacy_value.get("permission-policy-digest").is_none());

        fs::write(
            root.path().join(".fluent/tester.yaml"),
            "commands:\n  - command: echo ok\n    test_harness: shell-harness\n    permissions: [process-table-read]\n",
        )
        .unwrap();
        let requested = resolve_identity(root.path(), false, None).unwrap();
        assert_eq!(legacy.schema_version, 2);
        assert_eq!(requested.schema_version, 3);
        let requested_value = serde_json::to_value(&requested).unwrap();
        assert!(
            requested_value["permission-policy-digest"]
                .as_str()
                .unwrap()
                .starts_with("sha256:")
        );
        assert_ne!(
            identity_key(&legacy).unwrap(),
            identity_key(&requested).unwrap()
        );
        assert!(resolve_identity(root.path(), true, None).is_err());
    }

    #[test]
    fn permission_request_blocks_unsandboxed_baseline_routes_before_artifacts() {
        let root = tempfile::tempdir().unwrap();
        initialized_project(root.path());
        fs::write(
            root.path().join(".fluent/tester.yaml"),
            "commands:\n  - command: echo ok\n    test_harness: shell-harness\n    permissions: [process-table-read]\n",
        )
        .unwrap();
        let resolver = ContentResolver::new(Some(root.path()));
        let attempt_artifact = root.path().join("attempt-baseline");
        let landed_artifact = root.path().join("landed-baseline");

        let capture_error =
            capture(root.path(), root.path(), &attempt_artifact, true, &resolver).unwrap_err();
        assert!(
            capture_error
                .to_string()
                .contains("process-table-read requires the macOS sandbox")
        );
        assert!(!attempt_artifact.exists());

        let landed_error =
            capture_landed(root.path(), root.path(), &landed_artifact, true, &resolver)
                .unwrap_err();
        assert!(
            landed_error
                .to_string()
                .contains("process-table-read requires the macOS sandbox")
        );
        assert!(!landed_artifact.exists());
    }

    fn write_trustworthy_artifact(path: &Path, marker: &str) -> Result<TesterOutcome> {
        fs::create_dir_all(path.join("commands"))?;
        fs::write(path.join("commands/0-stdout.log"), marker)?;
        fs::write(
            path.join(RESULTS_FILE),
            serde_json::to_vec_pretty(&TesterResults {
                candidate_commit: None,
                commands: Vec::new(),
                tests: Vec::new(),
                summary: crate::tester::Summary {
                    total: 0,
                    pass: 0,
                    fail: 0,
                    skipped: 0,
                },
                error: None,
            })?,
        )?;
        Ok(TesterOutcome::Passed)
    }

    fn write_bound_artifact(path: &Path, commit: &str) {
        fs::create_dir_all(path.join("commands")).unwrap();
        fs::write(path.join("commands/0-stdout.log"), "ok").unwrap();
        fs::write(
            path.join(RESULTS_FILE),
            serde_json::to_vec_pretty(&TesterResults {
                candidate_commit: Some(commit.to_string()),
                commands: Vec::new(),
                tests: Vec::new(),
                summary: crate::tester::Summary {
                    total: 0,
                    pass: 0,
                    fail: 0,
                    skipped: 0,
                },
                error: None,
            })
            .unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn capability_baseline_requires_protected_provenance_before_publication_and_reuse() {
        let root = tempfile::tempdir().unwrap();
        initialized_project(root.path());
        fs::write(root.path().join(".fluent/tester.yaml"),
            "commands:\n  - command: 'true'\n    test_harness: shell-harness\n    permissions: [process-table-read]\n").unwrap();
        crate::git::run(
            root.path(),
            &["add", ".fluent/tester.yaml"],
            "stage request",
        )
        .unwrap();
        crate::git::run(
            root.path(),
            &["commit", "-qm", "Request process metadata"],
            "commit request",
        )
        .unwrap();
        let identity = resolve_identity(root.path(), false, Some("permission-session")).unwrap();
        let candidate = identity.source_commit.clone();
        let attempt = root.path().join("attempt");
        let outcome = capture_with(root.path(), &attempt, identity.clone(), |artifact| {
            write_bound_artifact(artifact, &candidate);
            Ok(TesterOutcome::Passed)
        });
        assert!(
            outcome.is_err(),
            "capability results without receipts were published"
        );

        // Recompute every mutable hash to isolate the missing authority check.
        let key = identity_key(&identity).unwrap();
        let cache = identity_cache_root(root.path(), &identity)
            .unwrap()
            .join(&key);
        let artifact = cache.join("artifact");
        fs::create_dir_all(&cache).unwrap();
        for case in [
            "missing-manifest",
            "missing-receipt",
            "forged-copy",
            "legacy-downgrade",
            "changed-command",
        ] {
            write_bound_artifact(&artifact, &candidate);
            let result: TesterResults = serde_json::from_value(serde_json::json!({
                "candidate_commit":candidate,"commands":[{
                    "command":"true","test_harness":"shell-harness","exit_code":0,"duration_ms":1,
                    "stdout_log":"commands/0-stdout.log","stderr_log":"commands/0-stderr.log",
                    "permission_receipt":"commands/0-permission-receipt.json","permission_receipt_id":"forged.json"
                }],"tests":[],"summary":{"total":0,"pass":0,"fail":0,"skipped":0},"error":null
            })).unwrap();
            fs::write(
                artifact.join(RESULTS_FILE),
                serde_json::to_vec(&result).unwrap(),
            )
            .unwrap();
            fs::write(artifact.join("commands/0-stderr.log"), "").unwrap();
            fs::write(
                artifact.join("commands/0-permission-receipt.json"),
                "forged",
            )
            .unwrap();
            if case != "missing-manifest" {
                let mut manifest = serde_json::json!({
                    "schema_version":2,"config_digest":identity.tester_config_digest,
                    "extractor_digest":identity.extractor_digest,"full_run":true,
                    "commands":[{"command":"true","test_harness":"shell-harness","permissions":["process-table-read"]}],
                    "original_command_indexes":[0],"permission_receipts":["forged.json"]
                });
                match case {
                    "missing-receipt" => {
                        manifest["permission_receipts"][0] = serde_json::Value::Null
                    }
                    "legacy-downgrade" => {
                        manifest["schema_version"] = serde_json::json!(1);
                        manifest["commands"][0]["permissions"] = serde_json::json!([]);
                    }
                    "changed-command" => {
                        manifest["commands"][0]["command"] = serde_json::json!("false")
                    }
                    _ => {}
                }
                fs::write(
                    artifact.join("canonical-run.json"),
                    serde_json::to_vec(&manifest).unwrap(),
                )
                .unwrap();
            }
            let manifest = BaselineManifest {
                identity: identity.clone(),
                cache_key: key.clone(),
                artifact_digest: digest_tree(&artifact).unwrap(),
                results_digest: digest_file(&artifact.join(RESULTS_FILE)).unwrap(),
            };
            fs::write(
                cache.join(MANIFEST_FILE),
                serde_json::to_vec(&manifest).unwrap(),
            )
            .unwrap();
            assert!(
                validated_manifest(root.path(), &cache, &identity, &key).is_err(),
                "{case}"
            );
            let final_identity = resolve_identity(root.path(), false, None).unwrap();
            let seed = FinalTesterSeed {
                identity: final_identity,
                artifact_digest: digest_tree_without_seed(&artifact).unwrap(),
                results_digest: digest_file(&artifact.join(RESULTS_FILE)).unwrap(),
            };
            fs::write(
                artifact.join(FINAL_TESTER_SEED_FILE),
                serde_json::to_vec(&seed).unwrap(),
            )
            .unwrap();
            assert!(
                reusable_final_tester(root.path(), &artifact, &candidate, false).is_err(),
                "{case}"
            );
            assert!(
                promote_final_tester(root.path(), root.path(), &artifact, &candidate, false)
                    .is_err(),
                "{case}"
            );
            assert!(
                record_final_tester_seed(root.path(), &artifact, false).is_err(),
                "{case}"
            );
            assert!(
                publish_existing_artifact(root.path(), &artifact, identity.clone()).is_err(),
                "{case}"
            );
        }
    }

    #[test]
    fn exact_final_tester_validation_does_not_publish_before_land() {
        let root = tempfile::tempdir().unwrap();
        let commit = initialized_project(root.path());
        let artifact = root.path().join("final-tester");
        write_bound_artifact(&artifact, &commit);
        record_final_tester_seed(root.path(), &artifact, true).unwrap();
        assert!(reusable_final_tester(root.path(), &artifact, &commit, true).unwrap());
        assert!(!root.path().join(CACHE_ROOT).exists());
        assert!(!reusable_final_tester(root.path(), &artifact, "another-tip", true).unwrap());
        fs::write(artifact.join("commands/0-stdout.log"), "tampered").unwrap();
        assert!(reusable_final_tester(root.path(), &artifact, &commit, true).is_err());
    }

    #[test]
    fn landed_baseline_replaces_next_attempt_capture() {
        let root = tempfile::tempdir().unwrap();
        let commit = initialized_project(root.path());
        let final_artifact = root.path().join("final-tester");
        write_bound_artifact(&final_artifact, &commit);
        record_final_tester_seed(root.path(), &final_artifact, true).unwrap();

        let disposition =
            promote_final_tester(root.path(), root.path(), &final_artifact, &commit, true).unwrap();
        assert!(matches!(
            disposition,
            Some(LandedSeedDisposition::Promoted { .. })
        ));

        let attempt_artifact = root.path().join("attempt-baseline");
        let resolver = ContentResolver::new(Some(root.path()));
        let reused = capture(root.path(), root.path(), &attempt_artifact, true, &resolver).unwrap();
        assert!(matches!(reused, CaptureDisposition::ReusedLanded { .. }));
        assert_eq!(
            fs::read(final_artifact.join(RESULTS_FILE)).unwrap(),
            fs::read(attempt_artifact.join(RESULTS_FILE)).unwrap()
        );
    }

    #[test]
    fn changed_landed_commit_refuses_final_tester_promotion() {
        let root = tempfile::tempdir().unwrap();
        let tested = initialized_project(root.path());
        let final_artifact = root.path().join("final-tester");
        write_bound_artifact(&final_artifact, &tested);
        record_final_tester_seed(root.path(), &final_artifact, true).unwrap();

        fs::write(root.path().join("changed"), "changed").unwrap();
        crate::git::run(root.path(), &["add", "changed"], "stage changed fixture").unwrap();
        crate::git::run(
            root.path(),
            &["commit", "-q", "-m", "change"],
            "commit changed fixture",
        )
        .unwrap();
        let landed =
            crate::git::run_stdout(root.path(), &["rev-parse", "HEAD"], "resolve changed head")
                .unwrap();

        assert_eq!(
            promote_final_tester(root.path(), root.path(), &final_artifact, &landed, true,)
                .unwrap(),
            None
        );
    }

    #[test]
    fn changed_final_tester_artifact_fails_before_promotion() {
        let root = tempfile::tempdir().unwrap();
        let commit = initialized_project(root.path());
        let final_artifact = root.path().join("final-tester");
        write_bound_artifact(&final_artifact, &commit);
        record_final_tester_seed(root.path(), &final_artifact, true).unwrap();
        fs::write(final_artifact.join("commands/0-stdout.log"), "changed").unwrap();

        let error = promote_final_tester(root.path(), root.path(), &final_artifact, &commit, true)
            .unwrap_err();
        assert!(error.to_string().contains("not trustworthy"));
        assert!(!root.path().join(CACHE_ROOT).join(LANDED_ROOT).is_dir());
    }

    #[test]
    fn matching_identity_reuses_one_trustworthy_run() {
        let root = tempfile::tempdir().unwrap();
        let runs = AtomicUsize::new(0);
        let first = root.path().join("attempt-a");
        let second = root.path().join("attempt-b");

        let captured = capture_with(root.path(), &first, identity("session", "abc"), |path| {
            runs.fetch_add(1, Ordering::SeqCst);
            write_trustworthy_artifact(path, "one")
        })
        .unwrap();
        let reused = capture_with(root.path(), &second, identity("session", "abc"), |path| {
            runs.fetch_add(1, Ordering::SeqCst);
            write_trustworthy_artifact(path, "two")
        })
        .unwrap();

        assert!(matches!(captured, CaptureDisposition::Published { .. }));
        assert!(matches!(reused, CaptureDisposition::Reused { .. }));
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        assert_eq!(
            fs::read(first.join(RESULTS_FILE)).unwrap(),
            fs::read(second.join(RESULTS_FILE)).unwrap()
        );
        assert!(first.join(PROVENANCE_FILE).is_file());
        assert!(second.join(PROVENANCE_FILE).is_file());
    }

    #[test]
    fn landed_capture_publishes_ordinary_test_failures() {
        let root = tempfile::tempdir().unwrap();
        let artifact = root.path().join("landing");
        let disposition = capture_with(root.path(), &artifact, landed_identity("abc"), |path| {
            write_trustworthy_artifact(path, "known failure")?;
            let results_path = path.join(RESULTS_FILE);
            let mut results: TesterResults = serde_json::from_slice(&fs::read(&results_path)?)?;
            results.summary.fail = 1;
            fs::write(results_path, serde_json::to_vec_pretty(&results)?)?;
            Ok(TesterOutcome::TestFailures)
        })
        .unwrap();

        assert!(matches!(disposition, CaptureDisposition::Published { .. }));
        assert!(root.path().join(CACHE_ROOT).join(LANDED_ROOT).is_dir());
    }

    #[test]
    fn concurrent_matching_requests_single_flight() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().to_path_buf();
        let runs = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(2));
        std::thread::scope(|scope| {
            for suffix in ["a", "b"] {
                let runs = Arc::clone(&runs);
                let barrier = Arc::clone(&barrier);
                let project_root = root_path.clone();
                let destination = project_root.join(format!("attempt-{suffix}"));
                scope.spawn(move || {
                    barrier.wait();
                    capture_with(
                        &project_root,
                        &destination,
                        identity("session", "abc"),
                        |path| {
                            runs.fetch_add(1, Ordering::SeqCst);
                            std::thread::sleep(std::time::Duration::from_millis(50));
                            write_trustworthy_artifact(path, suffix)
                        },
                    )
                    .unwrap();
                });
            }
        });
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        assert!(root_path.join("attempt-a").join(RESULTS_FILE).is_file());
        assert!(root_path.join("attempt-b").join(RESULTS_FILE).is_file());
    }

    #[test]
    fn each_identity_field_invalidates_reuse() {
        let root = tempfile::tempdir().unwrap();
        let runs = AtomicUsize::new(0);
        let original = identity("session", "abc");
        let mut identities = vec![original.clone()];
        let mut changed = original.clone();
        changed.schema_version += 1;
        identities.push(changed);
        let mut changed = original.clone();
        changed.scheduler_session = Some("other-session".to_string());
        identities.push(changed);
        let mut changed = original.clone();
        changed.source_commit = "def".to_string();
        identities.push(changed);
        let mut changed = original.clone();
        changed.tester_config_digest = "sha256:other-config".to_string();
        identities.push(changed);
        let mut changed = original.clone();
        changed.extractor_digest = "sha256:other-extractor".to_string();
        identities.push(changed);
        let mut changed = original.clone();
        changed.permission_policy_digest = Some("sha256:other-policy".to_string());
        identities.push(changed);
        let mut changed = original.clone();
        changed.fluent_version = "0.1.6 test".to_string();
        identities.push(changed);
        let mut changed = original.clone();
        changed.operating_system = "other-os".to_string();
        identities.push(changed);
        let mut changed = original.clone();
        changed.architecture = "other-arch".to_string();
        identities.push(changed);
        let mut changed = original;
        changed.sandboxed = true;
        identities.push(changed);

        for (index, identity) in identities.into_iter().enumerate() {
            let requests_permission = identity.permission_policy_digest.is_some();
            let outcome = capture_with(
                root.path(),
                &root.path().join(format!("attempt-{index}")),
                identity,
                |path| {
                    runs.fetch_add(1, Ordering::SeqCst);
                    write_trustworthy_artifact(path, &index.to_string())
                },
            );
            if requests_permission {
                assert!(
                    outcome.is_err(),
                    "unproven capability identity must not publish"
                );
            } else {
                outcome.unwrap();
            }
        }
        assert_eq!(runs.load(Ordering::SeqCst), 11);
    }

    #[test]
    fn harness_error_stays_attempt_local_and_is_not_cached() {
        let root = tempfile::tempdir().unwrap();
        let runs = AtomicUsize::new(0);
        for suffix in ["a", "b"] {
            let disposition = capture_with(
                root.path(),
                &root.path().join(format!("attempt-{suffix}")),
                identity("session", "abc"),
                |path| {
                    runs.fetch_add(1, Ordering::SeqCst);
                    fs::write(path.join(RESULTS_FILE), b"harness error")?;
                    Ok(TesterOutcome::HarnessError)
                },
            )
            .unwrap();
            assert_eq!(disposition, CaptureDisposition::AttemptLocal);
        }
        assert_eq!(runs.load(Ordering::SeqCst), 2);
        let key = identity_key(&identity("session", "abc")).unwrap();
        assert!(
            !root
                .path()
                .join(CACHE_ROOT)
                .join(SESSIONS_ROOT)
                .join(session_key("session"))
                .join(key)
                .exists()
        );
    }

    #[test]
    fn tampered_shared_artifact_fails_closed_without_rerun() {
        let root = tempfile::tempdir().unwrap();
        let attempt = root.path().join("attempt");
        let runs = AtomicUsize::new(0);
        let disposition = capture_with(root.path(), &attempt, identity("session", "abc"), |path| {
            runs.fetch_add(1, Ordering::SeqCst);
            write_trustworthy_artifact(path, "one")
        })
        .unwrap();
        let CaptureDisposition::Published { cache_key } = disposition else {
            panic!("first run must publish");
        };
        fs::write(
            root.path()
                .join(CACHE_ROOT)
                .join(SESSIONS_ROOT)
                .join(session_key("session"))
                .join(cache_key)
                .join("artifact")
                .join(RESULTS_FILE),
            b"tampered",
        )
        .unwrap();
        let second = capture_with(
            root.path(),
            &root.path().join("attempt-b"),
            identity("session", "abc"),
            |path| {
                runs.fetch_add(1, Ordering::SeqCst);
                write_trustworthy_artifact(path, "two")
            },
        );
        assert!(second.is_err());
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        assert!(attempt.join(RESULTS_FILE).is_file());
    }

    #[test]
    fn scheduler_session_cleanup_preserves_landed_baselines() {
        let root = tempfile::tempdir().unwrap();
        let cache_root = root.path().join(CACHE_ROOT).join(SESSIONS_ROOT);
        let landed = root.path().join(CACHE_ROOT).join(LANDED_ROOT).join("kept");
        fs::create_dir_all(cache_root.join("stale-session")).unwrap();
        fs::create_dir_all(&landed).unwrap();

        prepare_scheduler_session(root.path(), "current-session").unwrap();

        assert!(!cache_root.join("stale-session").exists());
        assert!(cache_root.join(session_key("current-session")).is_dir());
        assert!(landed.is_dir());

        finish_scheduler_session(root.path(), "current-session").unwrap();
        assert!(!cache_root.join(session_key("current-session")).exists());
        assert!(landed.is_dir());
    }
}
