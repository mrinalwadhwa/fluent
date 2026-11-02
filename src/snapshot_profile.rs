//! Benchmark-only observations; never retain facts used to project Work.
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
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
