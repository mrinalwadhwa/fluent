//! Read explicitly trusted SSH sources without granting remote metadata authority.
mod client;
mod protocol;
mod terminal;
pub use client::RemoteConnection;
pub use terminal::helper_terminal;
mod collection;
mod snapshot;
pub use collection::RemoteSources;
mod ssh;
pub use snapshot::{decode_snapshot, encode_snapshot, validate_descriptor};
pub use ssh::OpenSsh;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf};

pub const MAX_SNAPSHOT_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SshSource {
    pub name: String,
    pub host: String,
    pub project_root: PathBuf,
}
impl SshSource {
    pub fn id(&self) -> String {
        format!("ssh:{}", self.name)
    }
    pub fn validate(&self) -> Result<()> {
        identifier(&self.name)?;
        ensure!(
            !self.host.starts_with('-')
                && !self.host.is_empty()
                && self.host.len() <= 255
                && self
                    .host
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-@".contains(&b)),
            "SSH source {}: select an OpenSSH host alias",
            self.name
        );
        let path = self
            .project_root
            .to_str()
            .context("SSH project root must be UTF-8")?;
        ensure!(
            self.project_root.is_absolute()
                && path.len() <= 4096
                && !path.chars().any(char::is_control)
                && self
                    .project_root
                    .components()
                    .all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
                && !path.contains("//")
                && !path.ends_with('/')
                && !path.contains("/./"),
            "SSH source {}: use an explicit canonical absolute project root",
            self.name
        );
        Ok(())
    }
}

pub(crate) fn identifier(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= 255
            && value != "."
            && value != ".."
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
        "malformed remote identity"
    );
    Ok(())
}

/// Only the operator's home supplies trust; project configuration is never read.
pub fn load_sources(home: &Path) -> Result<Vec<SshSource>> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Config {
        schema_version: u32,
        ssh: Vec<SshSource>,
    }
    let path = home.join(".config/fluent/dashboard-sources.json");
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    ensure!(
        bytes.len() <= 64 * 1024,
        "SSH source configuration exceeds 64 KiB"
    );
    let config: Config = serde_json::from_slice(&bytes).context("reading user SSH sources")?;
    ensure!(
        config.schema_version == 1 && config.ssh.len() <= 32,
        "unsupported SSH source configuration"
    );
    let mut names = std::collections::BTreeSet::new();
    for source in &config.ssh {
        source.validate()?;
        ensure!(
            names.insert(source.name.clone()),
            "duplicate SSH source {}",
            source.name
        );
    }
    Ok(config.ssh)
}

pub fn helper_snapshot(project_root: &Path) -> Result<()> {
    use std::io::Write;
    ensure!(
        project_root.canonicalize()? == project_root,
        "use the remote canonical project root"
    );
    let snapshot = crate::dashboard::project::SourceSnapshot::load(project_root)?;
    std::io::stdout().write_all(&encode_snapshot(&snapshot)?)?;
    Ok(())
}
