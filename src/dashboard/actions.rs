//! Construct supported CLI actions from published state, never from guidance text.
use super::project::{ProjectWork, SourceSnapshot};
use anyhow::{Context, Result, ensure};
use std::{
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionKind {
    AttemptRun,
    CandidateShow,
    CandidateLand,
}
impl ActionKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::AttemptRun => "Run Attempt",
            Self::CandidateShow => "Show Candidate",
            Self::CandidateLand => "Land Candidate",
        }
    }
    pub fn effect(self) -> &'static str {
        match self {
            Self::AttemptRun => {
                "Advance the Attempt in the foreground; workers may change the candidate checkout."
            }
            Self::CandidateShow => "Inspect the Merge Candidate without changing Work.",
            Self::CandidateLand => {
                "Land this Merge Candidate into its target branch through Fluent's checks."
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OfferedAction {
    pub kind: ActionKind,
    pub source_id: String,
    pub checkout: PathBuf,
    pub host: String,
    pub work_item_id: String,
    pub attempt_id: Option<String>,
    pub candidate_id: Option<String>,
    pub candidate_commit: Option<String>,
    pub expected_action: String,
}
impl OfferedAction {
    pub fn remote(&self) -> bool {
        self.source_id.starts_with("ssh:")
    }
    pub fn args(&self) -> Vec<String> {
        let (group, verb) = match self.kind {
            ActionKind::AttemptRun => ("attempt", "run"),
            ActionKind::CandidateShow => ("merge-candidate", "show"),
            ActionKind::CandidateLand => ("merge-candidate", "land"),
        };
        let mut args = vec![group.into(), verb.into(), self.work_item_id.clone()];
        if let Some(id) = if self.kind == ActionKind::AttemptRun {
            &self.attempt_id
        } else {
            &self.candidate_id
        } {
            args.push(id.clone());
        }
        args
    }
    pub fn command_text(&self) -> String {
        format!("fluent {}", self.args().join(" "))
    }
    pub fn context(&self) -> String {
        format!(
            "{} · {} · {}",
            self.source_id,
            self.host,
            self.checkout.display()
        )
    }
    pub fn copy_text(&self) -> String {
        // The context is a comment, not a remote execution recipe. Flatten control
        // characters so pasted context cannot become an extra shell command.
        let context = self
            .context()
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect::<String>();
        format!("# Source checkout: {context}\n{}", self.command_text())
    }
}

fn identifier(id: &str) -> bool {
    !id.is_empty()
        && id != "-"
        && !id.starts_with('-')
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
        && crate::work_model::validate_work_item_id(id).is_ok()
}

pub fn offered(source: &SourceSnapshot, work: &ProjectWork) -> Vec<OfferedAction> {
    if !identifier(&work.status.id) || !work.status.compatibility_warnings.is_empty() {
        return vec![];
    }
    let kinds = match work.status.action.as_str() {
        "task-ready" | "learner-not-ready" if work.attempt_id.as_deref().is_none_or(identifier) => {
            vec![ActionKind::AttemptRun]
        }
        "merge-ready" if identifier(&work.status.merge_candidate) => {
            vec![ActionKind::CandidateShow, ActionKind::CandidateLand]
        }
        _ => vec![],
    };
    kinds
        .into_iter()
        .map(|kind| OfferedAction {
            kind,
            source_id: source.id.clone(),
            checkout: source.path.clone(),
            host: source.host.clone(),
            work_item_id: work.status.id.clone(),
            attempt_id: work.attempt_id.clone(),
            candidate_id: identifier(&work.status.merge_candidate)
                .then(|| work.status.merge_candidate.clone()),
            candidate_commit: work.candidate_commit.clone(),
            expected_action: work.status.action.clone(),
        })
        .collect()
}

#[derive(Clone, Debug)]
pub struct ActionMenu {
    pub actions: Vec<OfferedAction>,
    pub selected: usize,
}
impl ActionMenu {
    pub fn current(&self) -> &OfferedAction {
        &self.actions[self.selected]
    }
    pub fn values(&self) -> Vec<String> {
        let mut values = vec!["Fluent actions".into(), String::new()];
        for (index, action) in self.actions.iter().enumerate() {
            values.push(format!(
                "{} {}",
                if index == self.selected { ">" } else { " " },
                action.kind.label()
            ));
        }
        let action = self.current();
        values.extend([
            String::new(),
            action.command_text(),
            format!("Source checkout: {}", action.context()),
            format!("Work Item: {}", action.work_item_id),
            action.kind.effect().into(),
            String::new(),
            if action.remote() {
                "Copy only — run in the named remote checkout outside this dashboard.".into()
            } else {
                "Enter to choose this local command.".into()
            },
        ]);
        values
    }
}

pub fn copy_command(action: &OfferedAction) -> Result<()> {
    let mut child = Command::new("/usr/bin/pbcopy")
        .stdin(Stdio::piped())
        .spawn()
        .context("open clipboard; command remains visible for text selection")?;
    let write = child
        .stdin
        .take()
        .context("clipboard stdin")
        .and_then(|mut stdin| {
            stdin
                .write_all(action.copy_text().as_bytes())
                .map_err(Into::into)
        });
    let status = child.wait().context("wait for clipboard")?;
    write?;
    ensure!(
        status.success(),
        "clipboard exited with {status}; command remains visible for text selection"
    );
    Ok(())
}

/// Bind confirmation to the checked checkout and exact persisted Work record.
#[derive(Clone, Debug)]
pub struct PreparedAction {
    pub action: OfferedAction,
    source_identity: (u64, u64),
    item: crate::work_model::WorkItem,
}
impl PreparedAction {
    pub fn prepare(action: OfferedAction) -> Result<Self> {
        let (source_identity, item) = read_current(&action)?;
        Ok(Self {
            action,
            source_identity,
            item,
        })
    }
    pub fn values(&self) -> Vec<String> {
        vec![
            "Confirm local action".into(),
            String::new(),
            self.action.command_text(),
            format!("Source checkout: {}", self.action.context()),
            format!("Work Item: {}", self.action.work_item_id),
            format!("Effect: {}", self.action.kind.effect()),
            "The dashboard pauses while the command runs in the foreground.".into(),
            "Press y to confirm this command, or n/Esc to cancel.".into(),
        ]
    }
    pub fn validate(&self) -> Result<LocalCommand> {
        let (identity, item) = read_current(&self.action)?;
        ensure!(
            identity == self.source_identity,
            "Source checkout changed; select the action again"
        );
        ensure!(
            item == self.item,
            "Work Item or Merge Candidate changed; select the action again"
        );
        Ok(LocalCommand {
            executable: std::env::current_exe().context("locate current Fluent executable")?,
            args: self.action.args(),
            checkout: self.action.checkout.clone(),
        })
    }
}

fn read_current(action: &OfferedAction) -> Result<((u64, u64), crate::work_model::WorkItem)> {
    use std::os::unix::fs::MetadataExt;
    ensure!(!action.remote(), "SSH sources are copy only");
    let canonical = action
        .checkout
        .canonicalize()
        .context("read source checkout")?;
    ensure!(
        canonical == action.checkout && canonical.to_str() == Some(action.source_id.as_str()),
        "Source checkout changed; refresh and select the action again"
    );
    let top = crate::git::run_stdout(
        &canonical,
        &["rev-parse", "--show-toplevel"],
        "validate source checkout",
    )?;
    ensure!(
        PathBuf::from(top.trim()).canonicalize()? == canonical,
        "Source is no longer the selected checkout"
    );
    let metadata = std::fs::metadata(&canonical)?;
    let item = crate::work_model::WorkModelStore::new(&canonical)
        .read_work_item_snapshot(&action.work_item_id)
        .context("freshly read selected Work Item")?;
    let status = crate::work_status::summarize_work_item(&item, Some(&canonical));
    ensure!(
        status.compatibility_warnings.is_empty(),
        "Work Item requires a compatible Fluent version"
    );
    ensure!(
        status.action == action.expected_action,
        "Work Item action changed; refresh and select it again"
    );
    let attempt_id = crate::work_status::display_attempt(&item, Some(&canonical))
        .map(|attempt| attempt.id.clone());
    ensure!(
        attempt_id == action.attempt_id && attempt_id.is_some(),
        "Attempt identity changed; refresh and select it again"
    );
    let candidate = item
        .merge_candidates
        .iter()
        .find(|candidate| candidate.id == status.merge_candidate);
    ensure!(
        candidate.map(|candidate| &candidate.candidate_commit) == action.candidate_commit.as_ref(),
        "Merge Candidate commit changed; refresh and select it again"
    );
    ensure!(
        candidate.map(|candidate| &candidate.id) == action.candidate_id.as_ref(),
        "Merge Candidate identity changed; refresh and select it again"
    );
    if action.kind != ActionKind::AttemptRun {
        ensure!(candidate.is_some(), "Merge Candidate disappeared");
    }
    // Reapply the allowlist at the command boundary, independent of UI controls.
    ensure!(
        matches!(
            (action.kind, status.action.as_str()),
            (ActionKind::AttemptRun, "task-ready" | "learner-not-ready")
                | (
                    ActionKind::CandidateShow | ActionKind::CandidateLand,
                    "merge-ready"
                )
        ),
        "Action is no longer supported"
    );
    ensure!(
        identifier(&action.work_item_id)
            && action.attempt_id.as_deref().is_none_or(identifier)
            && action.candidate_id.as_deref().is_none_or(identifier),
        "Invalid command identity"
    );
    Ok(((metadata.dev(), metadata.ino()), item))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalCommand {
    pub executable: PathBuf,
    pub args: Vec<String>,
    pub checkout: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandOutcome {
    Succeeded,
    Failed(i32),
    Interrupted,
}
impl std::fmt::Display for CommandOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Succeeded => f.write_str("succeeded"),
            Self::Failed(code) => write!(f, "failed (exit {code})"),
            Self::Interrupted => f.write_str("interrupted"),
        }
    }
}
impl LocalCommand {
    pub fn run(&self, mut maintain: impl FnMut()) -> Result<CommandOutcome> {
        use std::os::unix::process::{CommandExt, ExitStatusExt};
        // The child shares the foreground process group. Ignore terminal interrupts
        // only in the dashboard; reset them in the child before exec so Ctrl-C
        // interrupts the command and still lets us restore the dashboard.
        let _interrupts = IgnoreInterrupts::new()?;
        let mut command = Command::new(&self.executable);
        command
            .args(&self.args)
            .current_dir(&self.checkout)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        unsafe {
            command.pre_exec(|| {
                libc::signal(libc::SIGINT, libc::SIG_DFL);
                libc::signal(libc::SIGQUIT, libc::SIG_DFL);
                Ok(())
            });
        }
        let mut child = command.spawn().context("run Fluent command")?;
        let status = loop {
            // Keep retained attachments alive without reading dashboard input
            // or drawing over the foreground command's inherited terminal.
            maintain();
            if let Some(status) = child.try_wait().context("wait for Fluent command")? {
                break status;
            }
            std::thread::sleep(std::time::Duration::from_millis(33));
        };
        Ok(if status.success() {
            CommandOutcome::Succeeded
        } else if status.signal().is_some() || matches!(status.code(), Some(130 | 143)) {
            CommandOutcome::Interrupted
        } else {
            CommandOutcome::Failed(status.code().unwrap_or(1))
        })
    }
}

struct IgnoreInterrupts(Vec<(i32, libc::sigaction)>);
impl IgnoreInterrupts {
    fn new() -> Result<Self> {
        let mut guard = Self(Vec::new());
        for signal in [libc::SIGINT, libc::SIGQUIT] {
            unsafe {
                let mut previous = std::mem::zeroed();
                let mut ignored: libc::sigaction = std::mem::zeroed();
                ignored.sa_sigaction = libc::SIG_IGN;
                libc::sigemptyset(&mut ignored.sa_mask);
                if libc::sigaction(signal, &ignored, &mut previous) != 0 {
                    return Err(std::io::Error::last_os_error().into());
                }
                guard.0.push((signal, previous));
            }
        }
        Ok(guard)
    }
}
impl Drop for IgnoreInterrupts {
    fn drop(&mut self) {
        for (signal, previous) in &self.0 {
            unsafe {
                libc::sigaction(*signal, previous, std::ptr::null_mut());
            }
        }
    }
}
