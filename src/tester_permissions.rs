//! Bind a configured Tester permission request to one clean candidate.

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::git;
use crate::tester::{self, TesterPermission};

// XNU includes the trailing zero in the consumer's four-element MIB name.
// Match that exact query; a prefix would also admit other numeric suffixes.
pub(crate) const PROCESS_TABLE_RULE: &str = "(allow sysctl-read (sysctl-name \"kern.proc.all.0\"))";

#[derive(Serialize)]
struct RequestIdentity<'a> {
    git_common_directory: &'a str,
    candidate_checkout: &'a str,
    candidate_commit: &'a str,
    config_digest: &'a str,
    extractor_digest: &'a str,
    command_index: usize,
    command: &'a str,
    working_directory: &'a str,
    capability: &'static str,
    host_platform: &'static str,
    policy_digest: &'a str,
}

#[derive(Serialize)]
pub struct DisplayedRequest {
    git_common_directory: String,
    candidate_checkout: String,
    candidate_commit: String,
    config_digest: String,
    extractor_digest: String,
    command_index: usize,
    command: String,
    working_directory: String,
    capability: &'static str,
    host_platform: &'static str,
    policy_digest: String,
    request_digest: String,
    metadata_exposure: &'static [&'static str],
    permission_scope: &'static str,
    approval_state: &'static str,
}

fn digest(bytes: impl AsRef<[u8]>) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

/// Identify the semantic Seatbelt policy independently of an ephemeral profile
/// path or a particular approval generation.
pub(crate) fn permission_policy_digest() -> String {
    let policy = [
        b"tester-process-table-policy-v2-bundled-protected\n".as_slice(),
        PROCESS_TABLE_RULE.as_bytes(),
        include_bytes!("../sandboxes/common.sb").as_slice(),
    ]
    .concat();
    #[cfg(feature = "test-support")]
    let policy = {
        let mut policy = policy;
        if let Some(revision) = std::env::var_os("FLUENT_TEST_TESTER_PERMISSION_POLICY_REVISION") {
            use std::os::unix::ffi::OsStrExt;
            policy.extend_from_slice(b"\ntest-policy-revision:");
            policy.extend_from_slice(revision.as_bytes());
        }
        policy
    };
    digest(policy)
}

fn committed_request_input(checkout: &Path, candidate: &str, relative: &str) -> Result<Vec<u8>> {
    let path = checkout.join(relative);
    ensure!(
        fs::symlink_metadata(&path)?.file_type().is_file(),
        "Tester request input is not a regular file: {relative}"
    );
    let bytes = fs::read(&path)?;
    let committed = git::run_raw(checkout, &["show", &format!("{candidate}:{relative}")])?;
    ensure!(
        committed.status.success() && committed.stdout == bytes,
        "Tester request input differs from committed {relative}"
    );
    Ok(bytes)
}

/// Inspect a request without persisting an approval or starting work.
pub fn show_request(
    checkout: &Path,
    candidate: &str,
    command_index: usize,
) -> Result<DisplayedRequest> {
    ensure!(
        candidate.len() == 40 && candidate.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "candidate must be a full Git SHA"
    );
    ensure!(
        checkout.is_absolute(),
        "candidate checkout must be absolute"
    );
    let canonical_checkout = fs::canonicalize(checkout).context("resolving candidate checkout")?;
    ensure!(
        checkout == canonical_checkout,
        "candidate checkout must use its canonical path"
    );
    let head = git::run_stdout(checkout, &["rev-parse", "HEAD"], "resolve candidate HEAD")?;
    ensure!(
        head == candidate,
        "candidate HEAD differs from requested SHA"
    );
    let status = git::run_stdout(
        checkout,
        &["status", "--porcelain", "--untracked-files=no"],
        "check tracked candidate state",
    )?;
    ensure!(status.is_empty(), "candidate has tracked changes");
    let common = git::run_stdout(
        checkout,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        "resolve candidate Git common directory",
    )?;
    let canonical_common = fs::canonicalize(&common).context("resolving Git common directory")?;
    ensure!(
        Path::new(&common) == canonical_common,
        "Git common directory must use its canonical path"
    );

    let config_bytes = committed_request_input(checkout, candidate, ".fluent/tester.yaml")?;
    let config = tester::read_tester_config(&checkout.join(".fluent/tester.yaml"))?;
    let command = config
        .commands
        .get(command_index)
        .context("Tester command index is absent")?;
    ensure!(
        command
            .permissions
            .contains(&TesterPermission::ProcessTableRead),
        "Tester command does not request process-table-read"
    );
    let extractor_bytes =
        committed_request_input(checkout, candidate, ".fluent/extract-tester-results")?;
    let config_digest = digest(config_bytes);
    let extractor_digest = digest(extractor_bytes);
    let policy_digest = permission_policy_digest();
    let candidate_checkout = canonical_checkout.to_string_lossy().into_owned();
    let git_common_directory = canonical_common.to_string_lossy().into_owned();
    let identity = RequestIdentity {
        git_common_directory: &git_common_directory,
        candidate_checkout: &candidate_checkout,
        candidate_commit: candidate,
        config_digest: &config_digest,
        extractor_digest: &extractor_digest,
        command_index,
        command: &command.command,
        working_directory: &candidate_checkout,
        capability: "process-table-read",
        host_platform: std::env::consts::OS,
        policy_digest: &policy_digest,
    };
    let request_digest = digest(serde_json::to_vec(&identity)?);
    let approval_state = approval_state(&request_digest)?;
    Ok(DisplayedRequest {
        git_common_directory,
        candidate_checkout: candidate_checkout.clone(),
        candidate_commit: candidate.to_owned(),
        config_digest,
        extractor_digest,
        command_index,
        command: command.command.clone(),
        working_directory: candidate_checkout,
        capability: "process-table-read",
        host_platform: std::env::consts::OS,
        policy_digest,
        request_digest,
        metadata_exposure: &[
            "process IDs and parent links",
            "process names",
            "user and group IDs",
            "terminal and runtime metadata",
        ],
        permission_scope: "read-only kern.proc.all.0 (four-element MIB) for this configured command and its descendants",
        approval_state,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ApprovalRecord {
    approval_id: String,
    request_digest: String,
    candidate_commit: String,
    command_index: usize,
    generation: usize,
    approved_at: String,
    revoked_at: Option<String>,
}

#[derive(Serialize)]
pub struct ApprovalResponse {
    approval_id: String,
    request_digest: String,
}

#[derive(Serialize)]
pub struct RevocationResponse {
    approval_id: String,
    revoked_at: String,
}

fn store_root() -> Result<PathBuf> {
    #[cfg(feature = "test-support")]
    if let Some(root) = std::env::var_os("FLUENT_TEST_TESTER_PERMISSION_STORE") {
        let root = PathBuf::from(root);
        ensure!(root.is_absolute(), "test permission store must be absolute");
        return Ok(root);
    }
    let home = crate::os::operator_home()?;
    ensure!(
        home == fs::canonicalize(&home)?,
        "operator home must be canonical"
    );
    Ok(home.join(".config/fluent/tester-permissions"))
}

/// Select host-only launch storage after checking every worker-writable root.
/// Approval preparation creates the store; launch preparation never creates it.
pub(crate) fn protected_profile_directory(writable_roots: &[PathBuf]) -> Result<PathBuf> {
    let root = store_root()?;
    validate_store_directory(&root)?;
    for writable in writable_roots {
        let writable = fs::canonicalize(writable)
            .with_context(|| format!("resolving worker-writable root {}", writable.display()))?;
        ensure!(
            !root.starts_with(&writable) && !writable.starts_with(&root),
            "protected Tester profile overlaps a worker-writable root"
        );
    }
    Ok(root)
}

/// Prepare baseline extraction storage without creating an approval record.
pub(crate) fn prepare_readiness_profile_directory(
    checkout: &Path,
    writable_roots: &[PathBuf],
) -> Result<PathBuf> {
    let root = store_root()?;
    prepare_store_directory(&root, checkout)?;
    protected_profile_directory(writable_roots)
}

fn validate_store_parent(root: &Path) -> Result<()> {
    let parent = root.parent().context("permission store has no parent")?;
    ensure!(
        parent == fs::canonicalize(parent)?,
        "permission store parent is not canonical"
    );
    let metadata = fs::symlink_metadata(parent)?;
    ensure!(
        metadata.is_dir() && metadata.uid() == unsafe { libc::geteuid() },
        "permission store parent has an unsafe owner or type"
    );
    ensure!(
        metadata.permissions().mode() & 0o022 == 0,
        "permission store parent is writable by others"
    );
    Ok(())
}

fn validate_store_directory(root: &Path) -> Result<()> {
    validate_store_parent(root)?;
    let metadata = fs::symlink_metadata(root)?;
    ensure!(metadata.is_dir(), "permission store is not a directory");
    ensure!(
        metadata.uid() == unsafe { libc::geteuid() },
        "permission store owner differs"
    );
    ensure!(
        metadata.permissions().mode() & 0o077 == 0,
        "permission store is accessible to others"
    );
    ensure!(
        root == fs::canonicalize(root)?,
        "permission store path is not canonical"
    );
    Ok(())
}

fn prepare_store_directory(root: &Path, checkout: &Path) -> Result<()> {
    ensure!(
        !root.starts_with(checkout) && !checkout.starts_with(root),
        "permission store overlaps candidate checkout"
    );
    let common = git::run_stdout(
        checkout,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        "resolve Git common directory",
    )?;
    let common = Path::new(&common);
    ensure!(
        !root.starts_with(common) && !common.starts_with(root),
        "permission store overlaps Git metadata"
    );
    let parent = root.parent().context("permission store has no parent")?;
    fs::create_dir_all(parent)?;
    validate_store_parent(root)?;
    match fs::symlink_metadata(root) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(root)?;
            fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;
        }
        Err(error) => return Err(error.into()),
    }
    validate_store_directory(root)
}

fn record_path(root: &Path, request_digest: &str) -> Result<PathBuf> {
    let hex = request_digest
        .strip_prefix("sha256:")
        .context("invalid request digest")?;
    ensure!(
        hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "invalid request digest"
    );
    Ok(root.join(format!("{hex}.json")))
}

fn read_records(root: &Path, request_digest: &str) -> Result<Vec<ApprovalRecord>> {
    match fs::symlink_metadata(root) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    }
    validate_store_directory(root)?;
    let path = record_path(root, request_digest)?;
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    ensure!(
        metadata.is_file(),
        "permission record is not a regular file"
    );
    ensure!(
        metadata.uid() == unsafe { libc::geteuid() },
        "permission record owner differs"
    );
    ensure!(
        metadata.permissions().mode() & 0o077 == 0,
        "permission record is accessible to others"
    );
    ensure!(
        metadata.nlink() == 1,
        "permission record has a writable alias"
    );
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)?;
    let opened = file.metadata()?;
    ensure!(
        opened.dev() == metadata.dev() && opened.ino() == metadata.ino() && opened.nlink() == 1,
        "permission record changed while opening"
    );
    use std::io::Read;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let records: Vec<ApprovalRecord> = serde_json::from_slice(&bytes)?;
    for (index, record) in records.iter().enumerate() {
        ensure!(
            record.request_digest == request_digest,
            "permission record request mismatch"
        );
        ensure!(
            record.generation == index + 1,
            "permission record generation mismatch"
        );
        ensure!(
            record.approval_id
                == digest(format!(
                    "tester-approval-v1\0{request_digest}\0{}",
                    record.generation
                )),
            "permission record identity mismatch"
        );
    }
    Ok(records)
}

fn approval_state(request_digest: &str) -> Result<&'static str> {
    let records = read_records(&store_root()?, request_digest)?;
    Ok(match records.last() {
        Some(record) if record.revoked_at.is_none() => "approved",
        Some(_) => "revoked",
        None => "absent",
    })
}

fn write_records(root: &Path, request_digest: &str, records: &[ApprovalRecord]) -> Result<()> {
    let path = record_path(root, request_digest)?;
    let mut file = tempfile::NamedTempFile::new_in(root)?;
    use std::io::Write;
    file.write_all(&serde_json::to_vec_pretty(records)?)?;
    file.flush()?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok(())
}

pub(crate) struct PermissionLease {
    _file: File,
}

#[derive(Clone)]
pub(crate) struct LaunchAuthority {
    pub approval_id: String,
    pub request_digest: String,
    pub candidate_commit: String,
    pub command_index: usize,
    pub config_digest: String,
    pub extractor_digest: String,
    pub policy_digest: String,
    pub admitted_at: String,
}

pub(crate) struct LaunchAdmission {
    _lease: PermissionLease,
    pub authority: LaunchAuthority,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AppliedPermissionReceipt {
    schema_version: u32,
    approval_id: String,
    request_digest: String,
    candidate_commit: String,
    command_index: usize,
    config_digest: String,
    extractor_digest: String,
    policy_digest: String,
    profile_digest: String,
    host_platform: String,
    execution_mode: String,
    admitted_at: String,
    settled_at: String,
    exit_code: i32,
    stdout_digest: String,
    stderr_digest: String,
}

/// Persist the applied authority outside worker-writable roots, then copy it
/// into the command artifact for inspection. Synthetic launches remain labeled.
pub(crate) fn record_applied_authority(
    authority: &LaunchAuthority,
    profile: &Path,
    exit_code: i32,
    stdout: &[u8],
    stderr: &[u8],
    artifact: &Path,
) -> Result<String> {
    let profile_bytes = fs::read(profile)?;
    ensure!(
        profile_bytes
            .windows(PROCESS_TABLE_RULE.len())
            .any(|window| window == PROCESS_TABLE_RULE.as_bytes()),
        "permission-bearing command lacks the approved process-table rule"
    );
    let receipt = AppliedPermissionReceipt {
        schema_version: 1,
        approval_id: authority.approval_id.clone(),
        request_digest: authority.request_digest.clone(),
        candidate_commit: authority.candidate_commit.clone(),
        command_index: authority.command_index,
        config_digest: authority.config_digest.clone(),
        extractor_digest: authority.extractor_digest.clone(),
        policy_digest: authority.policy_digest.clone(),
        profile_digest: digest(&profile_bytes),
        host_platform: std::env::consts::OS.to_string(),
        execution_mode: if cfg!(feature = "test-support") {
            "synthetic".to_string()
        } else {
            "macos-seatbelt".to_string()
        },
        admitted_at: authority.admitted_at.clone(),
        settled_at: crate::work_model::now_iso8601(),
        exit_code,
        stdout_digest: digest(stdout),
        stderr_digest: digest(stderr),
    };
    let bytes = serde_json::to_vec_pretty(&receipt)?;
    let root = store_root()?;
    validate_store_directory(&root)?;
    let mut protected = tempfile::Builder::new()
        .prefix("receipt-")
        .suffix(".json")
        .tempfile_in(&root)?;
    use std::io::Write;
    protected.write_all(&bytes)?;
    protected.as_file().sync_all()?;
    let (_, protected_path) = protected.keep()?;
    write_command_artifact(artifact, &bytes)?;
    Ok(protected_path
        .file_name()
        .context("protected permission receipt has no name")?
        .to_string_lossy()
        .into_owned())
}

pub(crate) fn write_command_artifact(path: &Path, bytes: &[u8]) -> Result<()> {
    use rustix::fs::{AtFlags, Mode, OFlags, openat, renameat, unlinkat};
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMPORARY: AtomicU64 = AtomicU64::new(0);

    let parent = path.parent().context("permission artifact has no parent")?;
    let named = fs::symlink_metadata(parent)?;
    ensure!(
        named.is_dir(),
        "permission artifact parent is not a directory"
    );
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(parent)?;
    let opened = directory.metadata()?;
    ensure!(
        opened.dev() == named.dev() && opened.ino() == named.ino(),
        "permission artifact parent changed while opening"
    );
    let name = path
        .file_name()
        .context("permission artifact has no name")?;
    match fs::symlink_metadata(path) {
        Ok(existing) => ensure!(
            existing.is_file() && existing.nlink() == 1,
            "command artifact is not a regular unaliased file"
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let temporary_name = format!(
        ".fluent-command-{}-{}",
        std::process::id(),
        NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed)
    );
    let file = openat(
        &directory,
        temporary_name.as_str(),
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::RUSR | Mode::WUSR,
    )?;
    let mut file = File::from(file);
    let result = (|| -> Result<()> {
        file.write_all(bytes)?;
        file.sync_all()?;
        renameat(&directory, temporary_name.as_str(), &directory, name)?;
        directory.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = unlinkat(&directory, temporary_name.as_str(), AtFlags::empty());
    }
    result
}

/// Read the host record by its bounded name and require an exact artifact copy.
/// An artifact digest alongside a worker-writable copy is never authority.
fn read_verified_receipt_at(
    root: &Path,
    receipt_id: &str,
    artifact_copy: &[u8],
) -> Result<AppliedPermissionReceipt> {
    let suffix = receipt_id
        .strip_prefix("receipt-")
        .and_then(|name| name.strip_suffix(".json"))
        .context("invalid protected permission receipt ID")?;
    ensure!(
        !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_alphanumeric()),
        "invalid protected permission receipt ID"
    );
    validate_store_directory(root)?;
    let path = root.join(receipt_id);
    let named = fs::symlink_metadata(&path)?;
    ensure!(
        named.is_file()
            && named.uid() == unsafe { libc::geteuid() }
            && named.permissions().mode() & 0o077 == 0
            && named.nlink() == 1,
        "protected permission receipt has unsafe ownership or type"
    );
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)?;
    let opened = file.metadata()?;
    ensure!(
        opened.dev() == named.dev() && opened.ino() == named.ino(),
        "protected permission receipt changed while opening"
    );
    use std::io::Read;
    let mut bytes = Vec::new();
    file.take(64 * 1024 + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 64 * 1024 && bytes == artifact_copy,
        "permission artifact copy differs from protected host receipt"
    );
    let receipt: AppliedPermissionReceipt = serde_json::from_slice(&bytes)?;
    ensure!(
        receipt.schema_version == 1,
        "unsupported permission receipt version"
    );
    Ok(receipt)
}

pub(crate) fn validate_manifest_permissions(
    candidate_commit: &str,
    manifest: &crate::tester::CanonicalRunManifest,
    results: &crate::tester::TesterResults,
    read: impl FnMut(&str) -> Result<Vec<u8>>,
) -> Result<()> {
    validate_manifest_permissions_with_authority(
        candidate_commit,
        manifest,
        results,
        read,
        store_root,
    )
}

fn validate_manifest_permissions_with_authority(
    candidate_commit: &str,
    manifest: &tester::CanonicalRunManifest,
    results: &tester::TesterResults,
    mut read: impl FnMut(&str) -> Result<Vec<u8>>,
    authority: impl Fn() -> Result<PathBuf>,
) -> Result<()> {
    let requests_permission = manifest
        .commands
        .iter()
        .any(|command| !command.permissions.is_empty());
    if !requests_permission {
        anyhow::ensure!(
            manifest.schema_version == 1
                && manifest.original_command_indexes.is_empty()
                && manifest.permission_receipts.is_empty()
                && results.commands.iter().all(|command| {
                    command.permission_receipt.is_none() && command.permission_receipt_id.is_none()
                }),
            "default Tester evidence has unexpected permission provenance"
        );
        return Ok(());
    }
    anyhow::ensure!(
        manifest.schema_version == 2
            && manifest.original_command_indexes.len() == manifest.commands.len()
            && manifest.permission_receipts.len() == manifest.commands.len()
            && results.commands.len() == manifest.commands.len()
            && results.candidate_commit.as_deref() == Some(candidate_commit),
        "permission-bearing Tester manifest lacks complete version 2 provenance"
    );
    for (index, (definition, result)) in manifest.commands.iter().zip(&results.commands).enumerate()
    {
        anyhow::ensure!(
            definition.command == result.command && definition.test_harness == result.test_harness,
            "permission-bearing Tester command identity differs from its settled result"
        );
        let original_index = manifest.original_command_indexes[index];
        anyhow::ensure!(
            !manifest.full_run || original_index == index,
            "canonical Tester changed a configured command index"
        );
        if definition.permissions.is_empty() {
            anyhow::ensure!(
                manifest.permission_receipts[index].is_none()
                    && result.permission_receipt.is_none()
                    && result.permission_receipt_id.is_none(),
                "unrequested command carries a permission receipt"
            );
            continue;
        }
        let receipt_id = manifest.permission_receipts[index]
            .as_deref()
            .context("permission-bearing command is missing a host receipt")?;
        anyhow::ensure!(
            result.permission_receipt_id.as_deref() == Some(receipt_id)
                && result.permission_receipt.as_deref()
                    == Some(format!("commands/{index}-permission-receipt.json").as_str()),
            "permission receipt identity differs from the command result"
        );
        let copy = read(&format!("commands/{index}-permission-receipt.json"))?;
        let stdout = read(&format!("commands/{index}-stdout.log"))?;
        let stderr = read(&format!("commands/{index}-stderr.log"))?;
        validate_applied_receipt_at(
            &authority()?,
            receipt_id,
            &copy,
            candidate_commit,
            original_index,
            &manifest.config_digest,
            &manifest.extractor_digest,
            result,
            &stdout,
            &stderr,
        )?;
    }
    Ok(())
}

/// Validate a selected recheck against the original configured identities and
/// its own protected execution receipts.
pub(crate) fn validate_recheck_permissions(
    candidate_commit: &str,
    source: &tester::CanonicalRunManifest,
    execution: &tester::CanonicalRunManifest,
    indexes: &[usize],
    results: &tester::TesterResults,
    read: impl FnMut(&str) -> Result<Vec<u8>>,
) -> Result<()> {
    ensure!(
        execution.config_digest == source.config_digest
            && execution.extractor_digest == source.extractor_digest
            && execution.original_command_indexes == indexes
            && execution.commands.len() == indexes.len()
            && execution
                .commands
                .iter()
                .zip(indexes)
                .all(|(command, index)| source.commands.get(*index) == Some(command)),
        "recheck permission identity differs from the configured command"
    );
    validate_manifest_permissions(candidate_commit, execution, results, read)
}

/// Require a full capability run matching the independently resolved configuration.
pub(crate) fn validate_run_permissions(
    candidate_commit: &str,
    config_digest: &str,
    extractor_digest: &str,
    commands: &[tester::TesterCommand],
    results: &tester::TesterResults,
    mut read: impl FnMut(&str) -> Result<Vec<u8>>,
) -> Result<()> {
    let manifest: tester::CanonicalRunManifest =
        serde_json::from_slice(&read("canonical-run.json")?)?;
    ensure!(
        manifest.full_run
            && manifest.commands == commands
            && manifest.config_digest == config_digest
            && manifest.extractor_digest == extractor_digest
            && manifest
                .commands
                .iter()
                .any(|command| !command.permissions.is_empty()),
        "capability evidence differs from its required configuration"
    );
    validate_manifest_permissions(candidate_commit, &manifest, results, read)
}

/// Credit a permission-bearing command only when the protected host receipt,
/// inspectable copy, current policy, approval history, and settled logs agree.
fn validate_applied_receipt_at(
    root: &Path,
    receipt_id: &str,
    artifact_copy: &[u8],
    candidate_commit: &str,
    command_index: usize,
    config_digest: &str,
    extractor_digest: &str,
    result: &tester::CommandResult,
    stdout: &[u8],
    stderr: &[u8],
) -> Result<()> {
    let receipt = read_verified_receipt_at(root, receipt_id, artifact_copy)?;
    validate_receipt_claims(
        &receipt,
        candidate_commit,
        command_index,
        config_digest,
        extractor_digest,
        result,
        stdout,
        stderr,
    )?;
    let records = read_records(root, &receipt.request_digest)?;
    let record = records
        .iter()
        .find(|record| {
            record.approval_id == receipt.approval_id
                && record.candidate_commit == candidate_commit
                && record.command_index == command_index
        })
        .context("permission receipt has no matching host approval history")?;
    let admitted = chrono::DateTime::parse_from_rfc3339(&receipt.admitted_at)?;
    let settled = chrono::DateTime::parse_from_rfc3339(&receipt.settled_at)?;
    let approved = chrono::DateTime::parse_from_rfc3339(&record.approved_at)?;
    ensure!(
        approved <= admitted && admitted <= settled,
        "permission receipt has impossible approval or settlement time"
    );
    if let Some(revoked_at) = &record.revoked_at {
        let revoked = chrono::DateTime::parse_from_rfc3339(revoked_at)?;
        ensure!(
            admitted <= revoked,
            "permission receipt claims admission after revocation"
        );
    }
    Ok(())
}

fn validate_receipt_claims(
    receipt: &AppliedPermissionReceipt,
    candidate_commit: &str,
    command_index: usize,
    config_digest: &str,
    extractor_digest: &str,
    result: &tester::CommandResult,
    stdout: &[u8],
    stderr: &[u8],
) -> Result<()> {
    ensure!(
        cfg!(target_os = "macos")
            && receipt.host_platform == "macos"
            && receipt.execution_mode == "macos-seatbelt",
        "permission receipt does not prove a production macOS Seatbelt launch"
    );
    ensure!(
        receipt.candidate_commit == candidate_commit
            && receipt.command_index == command_index
            && receipt.config_digest == config_digest
            && receipt.extractor_digest == extractor_digest
            && receipt.policy_digest == permission_policy_digest(),
        "permission receipt differs from candidate, command, or policy identity"
    );
    ensure!(
        receipt.exit_code == result.exit_code
            && receipt.stdout_digest == digest(stdout)
            && receipt.stderr_digest == digest(stderr),
        "permission receipt differs from settled command output"
    );
    Ok(())
}

fn acquire_permission_lease(root: &Path) -> Result<PermissionLease> {
    validate_store_directory(root)?;
    let lock = root.join("approval.lock");
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&lock)
        .context("open protected permission lease")?;
    rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive)
        .context("lock protected permission lease")?;
    let opened = file.metadata()?;
    let named = fs::symlink_metadata(&lock)?;
    ensure!(
        opened.is_file()
            && named.is_file()
            && opened.uid() == unsafe { libc::geteuid() }
            && opened.permissions().mode() & 0o077 == 0
            && opened.nlink() == 1
            && opened.dev() == named.dev()
            && opened.ino() == named.ino(),
        "permission lease is not a protected regular file"
    );
    Ok(PermissionLease { _file: file })
}

/// Record a displayed request as host authority without launching Tester.
pub fn approve_request(
    checkout: &Path,
    candidate: &str,
    command_index: usize,
    capability: &str,
    request_digest: &str,
) -> Result<ApprovalResponse> {
    ensure!(
        capability == "process-table-read",
        "unsupported Tester capability"
    );
    let shown = show_request(checkout, candidate, command_index)?;
    ensure!(
        shown.request_digest == request_digest,
        "request digest differs from current request"
    );
    let root = store_root()?;
    prepare_store_directory(&root, checkout)?;
    let _lease = acquire_permission_lease(&root)?;
    // Recheck after acquiring the host lease, before recording consent.
    let current = show_request(checkout, candidate, command_index)?;
    ensure!(
        current.request_digest == request_digest,
        "request changed before approval"
    );
    let mut records = read_records(&root, request_digest)?;
    if let Some(record) = records.last().filter(|record| record.revoked_at.is_none()) {
        return Ok(ApprovalResponse {
            approval_id: record.approval_id.clone(),
            request_digest: request_digest.to_owned(),
        });
    }
    let generation = records.len() + 1;
    let approval_id = digest(format!(
        "tester-approval-v1\0{request_digest}\0{generation}"
    ));
    records.push(ApprovalRecord {
        approval_id: approval_id.clone(),
        request_digest: request_digest.to_owned(),
        candidate_commit: candidate.to_owned(),
        command_index,
        generation,
        approved_at: crate::work_model::now_iso8601(),
        revoked_at: None,
    });
    write_records(&root, request_digest, &records)?;
    Ok(ApprovalResponse {
        approval_id,
        request_digest: request_digest.to_owned(),
    })
}

/// Check current host authority before preparing a permission-bearing launch.
/// The launch boundary repeats this check when it can retain the lease through
/// process creation; this preparation check alone never admits execution.
pub(crate) fn check_current_approval(
    checkout: &Path,
    candidate: &str,
    command_index: usize,
) -> Result<()> {
    let _lease = begin_launch_admission(checkout, candidate, command_index)?;
    Ok(())
}

/// Keep this lease until the selected command has been spawned. A completed
/// revocation cannot overtake a launch that has already passed admission.
pub(crate) fn begin_launch_admission(
    checkout: &Path,
    candidate: &str,
    command_index: usize,
) -> Result<LaunchAdmission> {
    let shown = show_request(checkout, candidate, command_index)?;
    ensure!(
        shown.approval_state == "approved",
        "unmet prerequisite: process-table-read has no current operator approval for command {command_index}"
    );
    let root = store_root()?;
    validate_store_directory(&root)?;
    let lease = acquire_permission_lease(&root)?;
    let current = show_request(checkout, candidate, command_index)?;
    ensure!(
        current.request_digest == shown.request_digest && current.approval_state == "approved",
        "unmet prerequisite: process-table-read approval changed for command {command_index}"
    );
    let records = read_records(&root, &current.request_digest)?;
    let record = records
        .last()
        .context("unmet prerequisite: approval record is absent")?;
    ensure!(
        record.revoked_at.is_none()
            && record.candidate_commit == candidate
            && record.command_index == command_index,
        "unmet prerequisite: process-table-read approval does not match command {command_index}"
    );
    Ok(LaunchAdmission {
        _lease: lease,
        authority: LaunchAuthority {
            approval_id: record.approval_id.clone(),
            request_digest: current.request_digest,
            candidate_commit: candidate.to_owned(),
            command_index,
            config_digest: current.config_digest,
            extractor_digest: current.extractor_digest,
            policy_digest: current.policy_digest,
            admitted_at: crate::work_model::now_iso8601(),
        },
    })
}

/// Revoke one approval while retaining its audit record.
pub fn revoke_approval(approval_id: &str) -> Result<RevocationResponse> {
    ensure!(
        approval_id.starts_with("sha256:")
            && approval_id.len() == 71
            && approval_id[7..]
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit()),
        "invalid approval ID"
    );
    let root = store_root()?;
    validate_store_directory(&root)?;
    let _lease = acquire_permission_lease(&root)?;
    for entry in fs::read_dir(&root)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }
        let name = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .context("invalid approval record name")?;
        if name.starts_with("receipt-") {
            continue;
        }
        let request_digest = format!("sha256:{name}");
        let mut records = read_records(&root, &request_digest)?;
        if let Some(record) = records
            .iter_mut()
            .find(|record| record.approval_id == approval_id)
        {
            let revoked_at = match &record.revoked_at {
                Some(revoked_at) => revoked_at.clone(),
                None => {
                    let revoked_at = crate::work_model::now_iso8601();
                    record.revoked_at = Some(revoked_at.clone());
                    write_records(&root, &request_digest, &records)?;
                    revoked_at
                }
            };
            return Ok(RevocationResponse {
                approval_id: approval_id.to_owned(),
                revoked_at,
            });
        }
    }
    anyhow::bail!("approval ID is not in the host authority store")
}

#[cfg(test)]
mod receipt_tests {
    use super::*;

    #[test]
    fn artifact_receipt_write_rejects_worker_path_aliases() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path().join("commands");
        fs::create_dir(&directory).unwrap();
        let target = temporary.path().join("host-target");
        fs::write(&target, b"original").unwrap();
        let artifact = directory.join("0-permission-receipt.json");
        std::os::unix::fs::symlink(&target, &artifact).unwrap();
        assert!(write_command_artifact(&artifact, b"receipt").is_err());
        assert_eq!(fs::read(&target).unwrap(), b"original");
        fs::remove_file(&artifact).unwrap();
        write_command_artifact(&artifact, b"receipt").unwrap();
        assert_eq!(fs::read(&artifact).unwrap(), b"receipt");
        write_command_artifact(&artifact, b"updated").unwrap();
        assert_eq!(fs::read(&artifact).unwrap(), b"updated");
        let aliased_parent = temporary.path().join("commands-alias");
        std::os::unix::fs::symlink(&directory, &aliased_parent).unwrap();
        assert!(write_command_artifact(&aliased_parent.join("other"), b"receipt").is_err());
    }

    #[test]
    fn receipt_claims_reject_synthetic_or_changed_result() {
        let mut receipt: AppliedPermissionReceipt = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "approval_id": "sha256:approval",
            "request_digest": "sha256:request",
            "candidate_commit": "candidate",
            "command_index": 1,
            "config_digest": "sha256:config",
            "extractor_digest": "sha256:extractor",
            "policy_digest": permission_policy_digest(),
            "profile_digest": "sha256:profile",
            "host_platform": "macos",
            "execution_mode": "synthetic",
            "admitted_at": "2026-10-11T00:00:00Z",
            "settled_at": "2026-10-11T00:00:01Z",
            "exit_code": 0,
            "stdout_digest": digest(b"ok"),
            "stderr_digest": digest(b"")
        }))
        .unwrap();
        let result: tester::CommandResult = serde_json::from_value(serde_json::json!({
            "command":"true", "test_harness":"shell-harness", "exit_code":0,
            "duration_ms":1, "stdout_log":"commands/0-stdout.log",
            "stderr_log":"commands/0-stderr.log"
        }))
        .unwrap();
        assert!(
            validate_receipt_claims(
                &receipt,
                "candidate",
                1,
                "sha256:config",
                "sha256:extractor",
                &result,
                b"ok",
                b""
            )
            .is_err()
        );
        receipt.execution_mode = "macos-seatbelt".to_string();
        if cfg!(target_os = "macos") {
            assert!(
                validate_receipt_claims(
                    &receipt,
                    "candidate",
                    1,
                    "sha256:config",
                    "sha256:extractor",
                    &result,
                    b"ok",
                    b""
                )
                .is_ok()
            );
        }
        assert!(
            validate_receipt_claims(
                &receipt,
                "candidate",
                1,
                "sha256:config",
                "sha256:extractor",
                &result,
                b"changed",
                b""
            )
            .is_err()
        );
    }

    #[test]
    fn protected_receipt_matches_revoked_approval_history() {
        if !cfg!(target_os = "macos") {
            return;
        }
        let temporary = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temporary.path()).unwrap().join("operator");
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let request_digest = digest(b"request");
        let approval_id = digest(format!("tester-approval-v1\0{request_digest}\01"));
        write_records(
            &root,
            &request_digest,
            &[ApprovalRecord {
                approval_id: approval_id.clone(),
                request_digest: request_digest.clone(),
                candidate_commit: "candidate".to_string(),
                command_index: 0,
                generation: 1,
                approved_at: "2026-10-11T00:00:00Z".to_string(),
                revoked_at: Some("2026-10-11T00:00:02Z".to_string()),
            }],
        )
        .unwrap();
        let id = "receipt-test.json";
        let bytes = serde_json::to_vec(&serde_json::json!({
            "schema_version": 1, "approval_id": approval_id,
            "request_digest": request_digest, "candidate_commit": "candidate",
            "command_index": 0, "config_digest": "sha256:config",
            "extractor_digest": "sha256:extractor",
            "policy_digest": permission_policy_digest(),
            "profile_digest": "sha256:profile", "host_platform": "macos",
            "execution_mode": "macos-seatbelt",
            "admitted_at": "2026-10-11T00:00:00Z",
            "settled_at": "2026-10-11T00:00:01Z", "exit_code": 0,
            "stdout_digest": digest(b"ok"), "stderr_digest": digest(b"")
        }))
        .unwrap();
        let path = root.join(id);
        fs::write(&path, &bytes).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let result: tester::CommandResult = serde_json::from_value(serde_json::json!({
            "command":"true", "test_harness":"shell-harness", "exit_code":0,
            "duration_ms":1, "stdout_log":"commands/0-stdout.log",
            "stderr_log":"commands/0-stderr.log"
        }))
        .unwrap();
        assert!(
            validate_applied_receipt_at(
                &root,
                id,
                &bytes,
                "candidate",
                0,
                "sha256:config",
                "sha256:extractor",
                &result,
                b"ok",
                b""
            )
            .is_ok()
        );
        assert!(
            validate_applied_receipt_at(
                &root,
                id,
                b"forged",
                "candidate",
                0,
                "sha256:config",
                "sha256:extractor",
                &result,
                b"ok",
                b""
            )
            .is_err()
        );
        assert!(
            validate_applied_receipt_at(
                &root,
                id,
                &bytes,
                "different",
                0,
                "sha256:config",
                "sha256:extractor",
                &result,
                b"ok",
                b""
            )
            .is_err()
        );
        // A valid failing execution does not attest to any replacement pass.
        let mut failing_result = result.clone();
        failing_result.exit_code = 1;
        failing_result.permission_receipt_id = Some(id.into());
        failing_result.permission_receipt = Some("commands/0-permission-receipt.json".into());
        let mut failing_receipt: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        failing_receipt["exit_code"] = serde_json::json!(1);
        let failing_bytes = serde_json::to_vec(&failing_receipt).unwrap();
        fs::write(&path, &failing_bytes).unwrap();
        let mut manifest: tester::CanonicalRunManifest = serde_json::from_value(serde_json::json!({
            "schema_version":2,"config_digest":"sha256:config","extractor_digest":"sha256:extractor",
            "commands":[{"command":"true","test_harness":"shell-harness","permissions":["process-table-read"]}],
            "full_run":true,"original_command_indexes":[0],"permission_receipts":[id]
        })).unwrap();
        let mut results: tester::TesterResults = serde_json::from_value(serde_json::json!({
            "candidate_commit":"candidate","commands":[failing_result],"tests":[],
            "summary":{"total":0,"pass":0,"fail":0,"skipped":0},"error":null
        }))
        .unwrap();
        let verify = |manifest: &tester::CanonicalRunManifest,
                      results: &tester::TesterResults,
                      copy: &[u8]| {
            validate_manifest_permissions_with_authority(
                "candidate",
                manifest,
                results,
                |name| {
                    Ok(match name {
                        "commands/0-permission-receipt.json" => copy.to_vec(),
                        "commands/0-stdout.log" => b"ok".to_vec(),
                        "commands/0-stderr.log" => vec![],
                        _ => panic!("unexpected artifact {name}"),
                    })
                },
                || Ok(root.clone()),
            )
        };
        verify(&manifest, &results, &failing_bytes).expect("valid original failing receipt");
        let mut replacement: tester::CanonicalRunManifest =
            serde_json::from_value(serde_json::to_value(&manifest).unwrap()).unwrap();
        replacement.permission_receipts[0] = None;
        assert!(
            validate_recheck_permissions(
                "candidate",
                &manifest,
                &replacement,
                &[0],
                &results,
                |_| panic!("a missing receipt must fail before reading artifacts")
            )
            .is_err()
        );
        replacement.original_command_indexes[0] = 1;
        assert!(
            validate_recheck_permissions(
                "candidate",
                &manifest,
                &replacement,
                &[0],
                &results,
                |_| panic!("a changed command index must fail before reading artifacts")
            )
            .is_err()
        );
        results.commands[0].exit_code = 0;
        assert!(
            verify(&manifest, &results, &failing_bytes).is_err(),
            "original receipt credited a replacement pass"
        );
        results.commands[0].exit_code = 1;
        manifest.permission_receipts[0] = None;
        assert!(verify(&manifest, &results, &failing_bytes).is_err());
        manifest.permission_receipts[0] = Some(id.into());
        assert!(verify(&manifest, &results, b"forged").is_err());
        let mut synthetic = failing_receipt;
        synthetic["execution_mode"] = serde_json::json!("synthetic");
        let synthetic = serde_json::to_vec(&synthetic).unwrap();
        fs::write(&path, &synthetic).unwrap();
        assert!(verify(&manifest, &results, &synthetic).is_err());
        fs::remove_file(&path).unwrap();
        assert!(verify(&manifest, &results, &failing_bytes).is_err());
        fs::write(&path, &bytes).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        write_records(
            &root,
            &request_digest,
            &[ApprovalRecord {
                approval_id,
                request_digest: request_digest.clone(),
                candidate_commit: "candidate".to_string(),
                command_index: 0,
                generation: 1,
                approved_at: "2026-10-10T00:00:00Z".to_string(),
                revoked_at: Some("2026-10-10T00:00:01Z".to_string()),
            }],
        )
        .unwrap();
        assert!(
            validate_applied_receipt_at(
                &root,
                id,
                &bytes,
                "candidate",
                0,
                "sha256:config",
                "sha256:extractor",
                &result,
                b"ok",
                b""
            )
            .is_err()
        );
    }

    #[test]
    fn protected_receipt_must_match_artifact_copy_and_private_file() {
        let temporary = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temporary.path()).unwrap().join("operator");
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let id = "receipt-test.json";
        let bytes = serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "approval_id": "sha256:approval",
            "request_digest": "sha256:request",
            "candidate_commit": "candidate",
            "command_index": 0,
            "config_digest": "sha256:config",
            "extractor_digest": "sha256:extractor",
            "policy_digest": "sha256:policy",
            "profile_digest": "sha256:profile",
            "host_platform": "macos",
            "execution_mode": "macos-seatbelt",
            "admitted_at": "2026-10-11T00:00:00Z",
            "settled_at": "2026-10-11T00:00:01Z",
            "exit_code": 0,
            "stdout_digest": "sha256:stdout",
            "stderr_digest": "sha256:stderr"
        }))
        .unwrap();
        let path = root.join(id);
        fs::write(&path, &bytes).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(read_verified_receipt_at(&root, id, &bytes).is_ok());
        assert!(read_verified_receipt_at(&root, id, b"forged artifact").is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_verified_receipt_at(&root, id, &bytes).is_err());
        fs::remove_file(&path).unwrap();
        let outside = root.join("outside.json");
        fs::write(&outside, &bytes).unwrap();
        std::os::unix::fs::symlink(&outside, &path).unwrap();
        assert!(read_verified_receipt_at(&root, id, &bytes).is_err());
    }
}
