//! Bind a configured Tester permission request to one clean candidate.

use anyhow::{Context, Result, ensure};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::Path;

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

    let config_bytes = fs::read(checkout.join(".fluent/tester.yaml"))?;
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
    let extractor_bytes = fs::read(checkout.join(".fluent/extract-tester-results"))?;
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
        approval_state: "absent",
    })
}
