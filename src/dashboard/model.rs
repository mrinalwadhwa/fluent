//! Share view state without coupling rendering to event handling.
use super::{
    layout,
    project::{self, Filter, RowKey},
    snapshot::DashboardSnapshot,
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pane {
    List,
    Detail,
}
/// Identify the pane that receives keys, independently of a retained Control lease.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputOwner {
    List,
    Details,
    Terminal,
}
/// Track reads in memory; never serialize receipt times into Work or SSH data.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReadHealth {
    pub pending: bool,
    pub last_success: Option<chrono::DateTime<chrono::Utc>>,
    pub error: Option<String>,
}

impl ReadHealth {
    pub fn warns(&self) -> bool {
        self.error.is_some()
            || self.pending
                && self.last_success.is_some_and(|time| {
                    chrono::Utc::now().signed_duration_since(time) >= chrono::Duration::seconds(10)
                })
    }

    pub fn complete(&mut self, error: Option<&str>) {
        self.pending = false;
        self.error = error.map(str::to_owned);
        if error.is_none() {
            self.last_success = Some(chrono::Utc::now());
        }
    }
}

pub struct App {
    pub local_read: ReadHealth,
    pub checking_sources: bool,
    pub visible_read_since: Option<std::time::Instant>,
    pub read_indicator: Option<usize>,
    pub source_reads: BTreeMap<String, ReadHealth>,
    pub configuration_errors: Vec<String>,
    pub snapshot: DashboardSnapshot,
    pub(super) all_work: bool,
    pub(super) selected: Option<String>,
    pub selected_key: Option<RowKey>,
    pub filter: Filter,
    pub expanded: BTreeSet<RowKey>,
    pub(super) selected_index: usize,
    pub pane: Pane,
    pub help: bool,
    pub errors: bool,
    pub error_scroll: u16,
    pub copy_mode: bool,
    pub list_scroll: u16,
    pub detail_scroll: u16,
    pub size: (u16, u16),
    pub(super) dirty: bool,
    pub(super) quit: bool,
    pub stale_error: Option<String>,
    pub terminal: Option<crate::terminal_view::Display>,
    pub show_details: bool,
}
impl App {
    pub fn source_warning(&self, source: &project::SourceSnapshot) -> bool {
        source.stale.is_some()
            || self
                .source_reads
                .get(&source.id)
                .is_some_and(ReadHealth::warns)
    }

    pub fn source_warning_count(&self) -> usize {
        let count = self
            .snapshot
            .sources
            .iter()
            .filter(|source| self.source_warning(source))
            .count();
        count.max(usize::from(self.local_read.error.is_some()))
    }

    /// Advance only visible read animation; background polls never animate.
    pub fn update_read_display(&mut self, now: std::time::Instant) {
        let pending =
            self.local_read.pending || self.source_reads.values().any(|read| read.pending);
        if !pending {
            self.visible_read_since = None;
            self.checking_sources = false;
        }
        let frame = if pending && self.checking_sources && self.source_warning_count() == 0 {
            let start = *self.visible_read_since.get_or_insert(now);
            let elapsed = now.saturating_duration_since(start).as_millis();
            (elapsed >= 500).then_some((elapsed / 250 % 4) as usize)
        } else {
            None
        };
        if self.read_indicator != frame {
            self.read_indicator = frame;
            self.dirty = true;
        }
    }

    pub fn source_status(&self) -> String {
        let count = self.source_warning_count();
        if count > 0 {
            format!("Read warning: {count} sources · e errors")
        } else if let Some(frame) = self.read_indicator {
            format!("Checking {}", ["|", "/", "-", "\\"][frame])
        } else {
            String::new()
        }
    }

    pub fn diagnostic_snapshot(&self) -> DashboardSnapshot {
        let mut snapshot = self.snapshot.clone();
        for source in &self.snapshot.sources {
            if self.source_warning(source) {
                if let Some(health) = self.source_reads.get(&source.id) {
                    snapshot.errors.push(format!(
                        "{}: {} · Last read UTC {}",
                        source.id,
                        health
                            .error
                            .as_deref()
                            .unwrap_or("STALE: pending read retains old snapshot"),
                        health
                            .last_success
                            .map(|time| time.to_rfc3339())
                            .unwrap_or_else(|| "unavailable".into())
                    ));
                }
            }
        }
        if let Some(error) = &self.stale_error {
            snapshot.errors.push(error.clone());
        }
        snapshot
    }

    pub fn input_owner(&self) -> InputOwner {
        if self.terminal_visible()
            && self
                .terminal
                .as_ref()
                .is_some_and(|display| display.focused)
        {
            InputOwner::Terminal
        } else if self.pane == Pane::List {
            InputOwner::List
        } else {
            InputOwner::Details
        }
    }

    /// Project the active navigation controls for both help and the footer.
    pub fn project_controls(&self) -> Vec<&'static str> {
        if self.input_owner() == InputOwner::Terminal {
            return vec!["Ctrl-] navigate"];
        }
        if self.help {
            return vec!["q quit", "?/Esc close help"];
        }
        if self.errors {
            return vec![
                "q quit",
                "e/Esc back",
                "↑/↓ scroll",
                "PgUp/PgDn page",
                "? help",
                "r refresh",
            ];
        }
        self.project_navigation_controls()
    }

    pub fn project_navigation_controls(&self) -> Vec<&'static str> {
        let mut controls = if self.input_owner() == InputOwner::List {
            let mut controls = vec!["q quit", "? help", "↑/↓ select", "→ details"];
            if self
                .selected_key
                .as_ref()
                .is_some_and(|key| key.task_id.is_some())
            {
                controls.push("t terminal");
            } else if self.selected_key.is_some() {
                controls.push("Enter workers");
            }
            controls
        } else if !self.show_details && self.terminal.is_some() && self.terminal_fits() {
            vec![
                "q quit",
                "←/Esc list",
                "i Control",
                "f full-screen",
                "? help",
                "t reconnect",
                "o Observe",
            ]
        } else {
            let mut controls = vec!["q quit", "? help", "←/Esc list", "↑/↓ scroll"];
            if self
                .selected_key
                .as_ref()
                .is_some_and(|key| key.task_id.is_some())
            {
                controls.push("t terminal");
            }
            controls
        };
        controls.extend([
            "Tab/⇧Tab view",
            "F next view",
            "a History/All",
            "e errors",
            "r refresh",
            "c copy",
        ]);
        // A retained attachment can still be explicitly focused from the list.
        if self.terminal.is_some() && self.terminal_fits() && self.input_owner() == InputOwner::List
        {
            controls.extend(["i Control", "f full-screen", "o Observe"]);
        }
        if self.terminal_fits()
            && self
                .terminal
                .as_ref()
                .is_some_and(|display| display.has_control)
            && (self.input_owner() == InputOwner::List || !self.show_details)
        {
            controls.push("] literal");
        }
        controls
    }

    pub fn selected_id(&self) -> Option<&str> {
        self.selected_key
            .as_ref()
            .map(|key| key.work_item_id.as_str())
            .or(self.selected.as_deref())
    }
    pub fn project_mode(&self) -> bool {
        self.snapshot.project_view
    }
    pub fn project_keys(&self) -> Vec<RowKey> {
        project::visible_keys(&self.snapshot.sources, self.filter, &self.expanded)
    }
    pub fn all_work(&self) -> bool {
        self.all_work
    }
    pub fn scroll_bounds(&self) -> layout::ScrollBounds {
        if self.project_mode() {
            layout::project_scroll_bounds(
                &self.snapshot.sources,
                self.filter,
                &self.expanded,
                self.selected_key.as_ref(),
                self.size,
                self.pane == Pane::Detail && self.show_details,
            )
        } else {
            layout::scroll_bounds(&self.snapshot, self.all_work, self.selected_id(), self.size)
        }
    }
}
