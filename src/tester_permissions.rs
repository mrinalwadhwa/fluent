//! Bind a configured Tester permission request to one clean candidate.

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::git;
use crate::tester::{self, TesterPermission};

const PROCESS_TABLE_RULE: &str = "(allow sysctl-read (sysctl-name \"kern.proc.all\"))";

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
    let policy_digest = digest(
        [
            b"tester-process-table-policy-v1\n".as_slice(),
            PROCESS_TABLE_RULE.as_bytes(),
            include_bytes!("../sandboxes/common.sb").as_slice(),
        ]
        .concat(),
    );
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
        permission_scope: "read-only kern.proc.all for this configured command and its descendants",
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
    let records: Vec<ApprovalRecord> = serde_json::from_slice(&fs::read(&path)?)?;
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

struct PermissionLease {
    _file: File,
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
    let shown = show_request(checkout, candidate, command_index)?;
    ensure!(
        shown.approval_state == "approved",
        "unmet prerequisite: process-table-read has no current operator approval for command {command_index}"
    );
    let root = store_root()?;
    validate_store_directory(&root)?;
    let _lease = acquire_permission_lease(&root)?;
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
    Ok(())
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
