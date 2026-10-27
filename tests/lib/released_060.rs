use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const PREPARE: &str = "run `FLUENT_RELEASED_060_BIN=\"$(bash tests/fixtures/prepare-released-060.sh)\"` then `export FLUENT_RELEASED_060_BIN` before the test";

#[derive(serde::Deserialize)]
struct Identity {
    version: String,
    assets: std::collections::HashMap<String, Asset>,
}

#[derive(serde::Deserialize)]
struct Asset {
    sha256: String,
}

pub fn copy_to(directory: &Path) -> Result<PathBuf> {
    let source = std::env::var_os("FLUENT_RELEASED_060_BIN")
        .with_context(|| format!("FLUENT_RELEASED_060_BIN is required; {PREPARE}"))?;
    copy_from(Path::new(&source), directory)
}

pub fn copy_from(source: &Path, directory: &Path) -> Result<PathBuf> {
    let identity: Identity = serde_json::from_str(include_str!("../fixtures/released-060.json"))
        .context("read committed released predecessor identity")?;
    let asset = identity
        .assets
        .get(std::env::consts::ARCH)
        .context("released v0.6.0 fixture requires macOS arm64 or x86_64")?;
    let bytes = fs::read(source).with_context(|| {
        format!(
            "cannot read FLUENT_RELEASED_060_BIN at {}; {PREPARE}",
            source.display()
        )
    })?;
    let digest = format!("{:x}", Sha256::digest(&bytes));
    ensure!(
        digest == asset.sha256,
        "released predecessor SHA-256 mismatch at {}: expected {}, got {}; {PREPARE}",
        source.display(),
        asset.sha256,
        digest
    );

    // Write exactly the bytes we checked, without reopening the shared source.
    let copy = directory.join("fluent-v0.6.0");
    fs::write(&copy, bytes).with_context(|| format!("copy predecessor to {}", copy.display()))?;
    fs::set_permissions(&copy, fs::Permissions::from_mode(0o755))?;
    let version = Command::new(&copy)
        .env("FLUENT_NO_UPDATE_CHECK", "1")
        .arg("version")
        .output()
        .with_context(|| {
            format!(
                "check predecessor version at {}; requires macOS on the selected architecture",
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
