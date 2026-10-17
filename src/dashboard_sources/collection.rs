use super::{OpenSsh, SshSource};
use crate::dashboard::project::SourceSnapshot;
use std::thread::JoinHandle;

struct Source {
    config: SshSource,
    snapshot: SourceSnapshot,
    pending: Option<JoinHandle<anyhow::Result<SourceSnapshot>>>,
}

/// Poll each SSH host independently so unreachable sources cannot stall input.
pub struct RemoteSources {
    sources: Vec<Source>,
    ssh: OpenSsh,
}
impl RemoteSources {
    pub fn new(config: Vec<SshSource>, ssh: OpenSsh) -> Self {
        Self {
            sources: config
                .into_iter()
                .map(|config| Source {
                    snapshot: SourceSnapshot {
                        schema_version: 1,
                        id: config.id(),
                        host: config.host.clone(),
                        path: config.project_root.clone(),
                        project: config.name.clone(),
                        work: Vec::new(),
                        queued_work_item_ids: Vec::new(),
                        diagnostics: Vec::new(),
                        stale: Some("Connecting to SSH source".into()),
                    },
                    config,
                    pending: None,
                })
                .collect(),
            ssh,
        }
    }
    pub fn refresh(&mut self) {
        for source in &mut self.sources {
            if source.pending.is_none() {
                let config = source.config.clone();
                let ssh = self.ssh.clone();
                match std::thread::Builder::new()
                    .name("ssh-snapshot".into())
                    .spawn(move || ssh.snapshot(&config))
                {
                    Ok(handle) => source.pending = Some(handle),
                    Err(error) => source.snapshot.stale = Some(error.to_string()),
                }
            }
        }
    }
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        for source in &mut self.sources {
            if source.pending.as_ref().is_some_and(|h| h.is_finished()) {
                let result = source
                    .pending
                    .take()
                    .unwrap()
                    .join()
                    .unwrap_or_else(|_| Err(anyhow::anyhow!("SSH snapshot reader stopped")));
                match result {
                    Ok(snapshot) => source.snapshot = snapshot,
                    Err(error) => source.snapshot.stale = Some(format!("{error:#}")),
                }
                changed = true;
            }
        }
        changed
    }
    pub fn snapshots(&self) -> Vec<SourceSnapshot> {
        self.sources.iter().map(|s| s.snapshot.clone()).collect()
    }
}
impl Drop for RemoteSources {
    fn drop(&mut self) {
        for source in &mut self.sources {
            if let Some(handle) = source.pending.take() {
                let _ = handle.join();
            }
        }
    }
}
