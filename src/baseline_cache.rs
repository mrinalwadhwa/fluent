//! Scheduler-session cache for immutable pre-write Tester baselines.
//!
//! Direct Attempts retain Attempt-local capture. Scheduler children receive one
//! opaque session id, allowing Work Items with the same source and Tester
//! boundary to share one host-owned run within that scheduler lifetime.

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
const PROVENANCE_FILE: &str = "baseline-provenance.json";
const MANIFEST_FILE: &str = "manifest.json";
const RESULTS_FILE: &str = "tester-results.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct BaselineIdentity {
    schema_version: u32,
    scheduler_session: String,
    source_commit: String,
    tester_config_digest: String,
    extractor_digest: String,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CaptureDisposition {
    AttemptLocal,
    Published { cache_key: String },
    Reused { cache_key: String },
}

/// Start one elected scheduler's cache, removing evidence left by an interrupted
/// predecessor before any child can publish a new baseline.
pub(crate) fn prepare_scheduler_session(project_root: &Path, session: &str) -> Result<()> {
    let root = project_root.join(CACHE_ROOT);
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
    let session_dir = project_root.join(CACHE_ROOT).join(session_key(session));
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

    let identity = resolve_identity(candidate_workspace, no_sandbox, &session)?;
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
    let cache_root = project_root
        .join(CACHE_ROOT)
        .join(session_key(&identity.scheduler_session));
    let cache_dir = cache_root.join(&cache_key);
    let lock_path = cache_root.join("locks").join(format!("{cache_key}.lock"));
    let _lease = crate::lease::acquire_blocking(&lock_path)
        .with_context(|| format!("acquire shared baseline lock {}", lock_path.display()))?;

    if let Some(manifest) = validated_manifest(&cache_dir, &identity, &cache_key)? {
        copy_tree(&cache_dir.join("artifact"), attempt_artifact_dir)?;
        write_provenance(attempt_artifact_dir, &manifest, "reused-scheduler-baseline")?;
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
    write_provenance(
        attempt_artifact_dir,
        &manifest,
        "captured-scheduler-baseline",
    )?;
    Ok(CaptureDisposition::Published { cache_key })
}

fn resolve_identity(
    candidate_workspace: &Path,
    no_sandbox: bool,
    scheduler_session: &str,
) -> Result<BaselineIdentity> {
    Ok(BaselineIdentity {
        schema_version: 1,
        scheduler_session: scheduler_session.to_string(),
        source_commit: crate::git::run_stdout(
            candidate_workspace,
            &["rev-parse", "HEAD"],
            "resolve shared baseline source commit",
        )?,
        tester_config_digest: digest_file(&candidate_workspace.join(".fluent/tester.yaml"))?,
        extractor_digest: digest_file(&candidate_workspace.join(".fluent/extract-tester-results"))?,
        fluent_version: crate::version::version_tag(),
        operating_system: std::env::consts::OS.to_string(),
        architecture: std::env::consts::ARCH.to_string(),
        sandboxed: !no_sandbox,
    })
}

fn identity_key(identity: &BaselineIdentity) -> Result<String> {
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(identity)?)
    ))
}

fn validated_manifest(
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
    Ok(Some(manifest))
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

    fn identity(session: &str, commit: &str) -> BaselineIdentity {
        BaselineIdentity {
            schema_version: 1,
            scheduler_session: session.to_string(),
            source_commit: commit.to_string(),
            tester_config_digest: "sha256:config".to_string(),
            extractor_digest: "sha256:extractor".to_string(),
            fluent_version: "0.1.5 test".to_string(),
            operating_system: "test-os".to_string(),
            architecture: "test-arch".to_string(),
            sandboxed: false,
        }
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
        changed.scheduler_session = "other-session".to_string();
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
            capture_with(
                root.path(),
                &root.path().join(format!("attempt-{index}")),
                identity,
                |path| {
                    runs.fetch_add(1, Ordering::SeqCst);
                    write_trustworthy_artifact(path, &index.to_string())
                },
            )
            .unwrap();
        }
        assert_eq!(runs.load(Ordering::SeqCst), 10);
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
    fn scheduler_session_preparation_clears_stale_cache_and_finish_removes_current_cache() {
        let root = tempfile::tempdir().unwrap();
        let cache_root = root.path().join(CACHE_ROOT);
        fs::create_dir_all(cache_root.join("stale-session")).unwrap();

        prepare_scheduler_session(root.path(), "current-session").unwrap();

        assert!(!cache_root.join("stale-session").exists());
        assert!(cache_root.join(session_key("current-session")).is_dir());

        finish_scheduler_session(root.path(), "current-session").unwrap();
        assert!(!cache_root.join(session_key("current-session")).exists());
    }
}
