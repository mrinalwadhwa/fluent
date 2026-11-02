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
    completed: Vec<SourceSnapshot>,
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
            completed: Vec::new(),
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
                    Err(error) => {
                        source.snapshot.stale = Some(error.to_string());
                        self.completed.push(source.snapshot.clone());
                    }
                }
            }
        }
    }
    pub fn poll(&mut self) -> bool {
        !self.poll_results().is_empty()
    }

    /// Return only completed sources so pending peers do not gain receipt times.
    pub fn poll_results(&mut self) -> Vec<SourceSnapshot> {
        let mut completed = std::mem::take(&mut self.completed);
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
                completed.push(source.snapshot.clone());
            }
        }
        completed
    }

    pub fn pending_ids(&self) -> Vec<String> {
        self.sources
            .iter()
            .filter(|source| source.pending.is_some())
            .map(|source| source.snapshot.id.clone())
            .collect()
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn completed_sources_exclude_pending_peers_and_report_equal_reads() {
        let config = ["ready", "blocked"].map(|name| SshSource {
            name: name.into(),
            host: name.into(),
            project_root: "/project".into(),
        });
        let mut remote = RemoteSources::new(config.into(), Default::default());
        let mut ready = remote.sources[0].snapshot.clone();
        ready.stale = None;
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        remote.sources[1].pending = Some(std::thread::spawn(move || {
            release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            anyhow::bail!("blocked host failed")
        }));
        // Repeat the same successful value; every completion needs its own receipt.
        for _ in 0..2 {
            let next = ready.clone();
            remote.sources[0].pending = Some(std::thread::spawn(move || Ok(next)));
            assert_eq!(remote.pending_ids(), vec!["ssh:ready", "ssh:blocked"]);
            let deadline = Instant::now() + Duration::from_secs(5);
            let completed = loop {
                let completed = remote.poll_results();
                if !completed.is_empty() {
                    break completed;
                }
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(1));
            };
            assert_eq!(completed, vec![ready.clone()]);
            assert_eq!(remote.pending_ids(), vec!["ssh:blocked"]);
            assert!(remote.poll_results().is_empty());
        }
        release_tx.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let failed = loop {
            let completed = remote.poll_results();
            if !completed.is_empty() {
                break completed;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        };
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].id, "ssh:blocked");
        assert_eq!(failed[0].stale.as_deref(), Some("blocked host failed"));
        assert_eq!(remote.snapshots()[0], ready);
    }
}
