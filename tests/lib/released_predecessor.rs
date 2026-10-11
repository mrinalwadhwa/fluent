use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const PREPARE: &str = "run `FLUENT_RELEASED_050_BIN=\"$(bash tests/fixtures/prepare-released-predecessor.sh)\"` then `export FLUENT_RELEASED_050_BIN` before the test";

#[derive(serde::Deserialize)]
struct Identity {
    version: String,
    sha256: String,
}

pub fn copy_to(directory: &Path) -> Result<PathBuf> {
    let source = std::env::var_os("FLUENT_RELEASED_050_BIN")
        .with_context(|| format!("FLUENT_RELEASED_050_BIN is required; {PREPARE}"))?;
    copy_from(Path::new(&source), directory)
}

pub fn copy_from(source: &Path, directory: &Path) -> Result<PathBuf> {
    let identity: Identity =
        serde_json::from_str(include_str!("../fixtures/released-predecessor.json"))
            .context("read committed released predecessor identity")?;
    let bytes = fs::read(source).with_context(|| {
        format!(
            "cannot read FLUENT_RELEASED_050_BIN at {}; {PREPARE}",
            source.display()
        )
    })?;
    let digest = format!("{:x}", Sha256::digest(&bytes));
    ensure!(
        digest == identity.sha256,
        "released predecessor SHA-256 mismatch at {}: expected {}, got {}; {PREPARE}",
        source.display(),
        identity.sha256,
        digest
    );

    // Write exactly the bytes we checked, without reopening the shared source.
    let copy = directory.join("fluent-v0.5.0");
    fs::write(&copy, bytes).with_context(|| format!("copy predecessor to {}", copy.display()))?;
    fs::set_permissions(&copy, fs::Permissions::from_mode(0o755))?;
    let version = Command::new(&copy)
        .env("FLUENT_NO_UPDATE_CHECK", "1")
        .arg("version")
        .output()
        .with_context(|| {
            format!(
                "check predecessor version at {}; requires macOS on Apple Silicon",
                copy.display()
            )
        })?;
    ensure!(
        version.status.success()
            && String::from_utf8_lossy(&version.stdout).trim() == identity.version,
        "released predecessor version check failed at {}: expected {:?}, got {:?} ({}): {}",
        copy.display(),
        identity.version,
        String::from_utf8_lossy(&version.stdout).trim(),
        version.status,
        String::from_utf8_lossy(&version.stderr)
    );
    Ok(copy)
}
