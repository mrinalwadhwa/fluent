//! Benchmark-only observations; never retain facts used to project Work.
use anyhow::{Context, ensure};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

#[derive(Debug, Default, serde::Serialize)]
pub struct Profile {
    pub stages_ms: BTreeMap<String, f64>,
    pub operations: BTreeMap<String, usize>,
    pub git: Vec<GitCall>,
}

#[derive(Debug, serde::Serialize)]
pub struct GitCall {
    pub stage: String,
    pub cwd: PathBuf,
    pub args: Vec<String>,
    pub elapsed_ms: f64,
}

struct Active {
    profile: Profile,
    stage: &'static str,
    since: Instant,
}

thread_local! {
    static ACTIVE: RefCell<Option<Active>> = const { RefCell::new(None) };
}

/// Capture one synchronous read on this thread, independently of other readers.
pub fn capture<T>(read: impl FnOnce() -> T) -> (T, Profile) {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            ACTIVE.with(|slot| slot.borrow_mut().take());
        }
    }
    ACTIVE.with(|slot| {
        let mut slot = slot.borrow_mut();
        assert!(slot.is_none(), "snapshot profiling cannot nest");
        *slot = Some(Active {
            profile: Profile::default(),
            stage: "setup",
            since: Instant::now(),
        });
    });
    let _reset = Reset;
    let value = read();
    mark("finished");
    let profile = ACTIVE.with(|slot| slot.borrow_mut().take().unwrap().profile);
    (value, profile)
}

/// End the previous stage and start the next, accumulating repeated stages.
pub(crate) fn mark(stage: &'static str) {
    ACTIVE.with(|slot| {
        if let Some(active) = slot.borrow_mut().as_mut() {
            *active
                .profile
                .stages_ms
                .entry(active.stage.into())
                .or_default() += active.since.elapsed().as_secs_f64() * 1000.0;
            active.stage = stage;
            active.since = Instant::now();
        }
    });
}

pub(crate) fn operation(name: &str) {
    ACTIVE.with(|slot| {
        if let Some(active) = slot.borrow_mut().as_mut() {
            *active.profile.operations.entry(name.into()).or_default() += 1;
        }
    });
}

pub(crate) fn git(command: &Command, started: Instant) {
    ACTIVE.with(|slot| {
        if let Some(active) = slot.borrow_mut().as_mut() {
            active.profile.git.push(GitCall {
                stage: active.stage.into(),
                cwd: command
                    .get_current_dir()
                    .unwrap_or(std::path::Path::new("."))
                    .into(),
                args: command
                    .get_args()
                    .map(|arg| arg.to_string_lossy().into_owned())
                    .collect(),
                elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
            });
        }
    });
}

/// Hash Work names, types, links, and bytes without following links or writing state.
pub fn input_digest(root: &Path) -> anyhow::Result<String> {
    fn visit(path: &Path, digest: &mut Sha256) -> anyhow::Result<()> {
        let metadata = path
            .symlink_metadata()
            .with_context(|| format!("inspect input {}", path.display()))?;
        if metadata.is_dir() {
            digest.update(b"directory\0");
            let mut entries = std::fs::read_dir(path)
                .with_context(|| format!("enumerate input directory {}", path.display()))?
                .map(|entry| entry.map(|entry| entry.path()))
                .collect::<Result<Vec<_>, _>>()
                .with_context(|| format!("read input directory entry in {}", path.display()))?;
            entries.sort();
            for entry in entries {
                let name = entry.file_name().unwrap().as_encoded_bytes();
                digest.update(name.len().to_le_bytes());
                digest.update(name);
                visit(&entry, digest)?;
            }
            digest.update(b"end-directory\0");
        } else if metadata.is_symlink() {
            digest.update(b"link\0");
            let target = std::fs::read_link(path)
                .with_context(|| format!("read input link {}", path.display()))?;
            let bytes = target.as_os_str().as_encoded_bytes();
            digest.update(bytes.len().to_le_bytes());
            digest.update(bytes);
        } else {
            ensure!(metadata.is_file(), "unsupported input {}", path.display());
            digest.update(b"file\0");
            let bytes = std::fs::read(path)
                .with_context(|| format!("read input file {}", path.display()))?;
            digest.update(bytes.len().to_le_bytes());
            digest.update(bytes);
        }
        Ok(())
    }
    let mut digest = Sha256::new();
    visit(&root.join(".fluent/work"), &mut digest)?;
    Ok(format!("{:x}", digest.finalize()))
}
