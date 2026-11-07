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
            Self::AttemptRun => "Attempt run",
            Self::CandidateShow => "Candidate show",
            Self::CandidateLand => "Candidate land",
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
            candidate_id: (kind != ActionKind::AttemptRun)
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
