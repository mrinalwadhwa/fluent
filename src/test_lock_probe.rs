//! Scoped synchronization probes for lock-contention unit tests.

use std::collections::HashSet;
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use std::time::Duration;

struct ProbeState {
    reached: HashSet<(String, String)>,
    released: bool,
}

struct ProbeInner {
    channel: String,
    target: String,
    pause_at: Option<(String, String)>,
    state: Mutex<ProbeState>,
    changed: Condvar,
}

fn registry() -> &'static Mutex<Vec<Weak<ProbeInner>>> {
    static REGISTRY: OnceLock<Mutex<Vec<Weak<ProbeInner>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Vec::new()))
}

pub(crate) struct ScopedLockProbe {
    inner: Arc<ProbeInner>,
}

impl ScopedLockProbe {
    pub(crate) fn install(channel: &str, target: &str, pause_at: Option<(&str, &str)>) -> Self {
        let inner = Arc::new(ProbeInner {
            channel: channel.to_string(),
            target: target.to_string(),
            pause_at: pause_at.map(|(actor, phase)| (actor.to_string(), phase.to_string())),
            state: Mutex::new(ProbeState {
                reached: HashSet::new(),
                released: false,
            }),
            changed: Condvar::new(),
        });
        registry()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(Arc::downgrade(&inner));
        Self { inner }
    }

    pub(crate) fn wait_for(&self, actor: &str, phase: &str) -> bool {
        let expected = (actor.to_string(), phase.to_string());
        let state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (state, _) = self
            .inner
            .changed
            .wait_timeout_while(state, Duration::from_secs(10), |state| {
                !state.reached.contains(&expected)
            })
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.reached.contains(&expected)
    }

    pub(crate) fn release(&self) {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.released = true;
        self.inner.changed.notify_all();
    }
}

impl Drop for ScopedLockProbe {
    fn drop(&mut self) {
        self.release();
    }
}

pub(crate) fn reach(channel: &str, target: &str, actor: &str, phase: &str) {
    let probes = {
        let mut registry = registry()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut probes = Vec::new();
        registry.retain(|probe| {
            let Some(probe) = probe.upgrade() else {
                return false;
            };
            if probe.channel == channel && probe.target == target {
                probes.push(probe);
            }
            true
        });
        probes
    };

    for probe in probes {
        let point = (actor.to_string(), phase.to_string());
        let mut state = probe
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.reached.insert(point.clone());
        probe.changed.notify_all();
        if probe.pause_at.as_ref() == Some(&point) {
            let (guard, _) = probe
                .changed
                .wait_timeout_while(state, Duration::from_secs(10), |state| !state.released)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state = guard;
        }
        drop(state);
    }
}
