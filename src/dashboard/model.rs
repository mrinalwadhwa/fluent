//! Share view state without coupling rendering to event handling.
use super::{
    layout,
    project::{self, Filter, RowKey},
    snapshot::DashboardSnapshot,
};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pane {
    List,
    Detail,
}
/// Navigate Work and workers independently of a retained terminal attachment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectPage {
    List,
    Overview,
    Worker,
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
    pub selected_worker: Option<RowKey>,
    pub page: ProjectPage,
    pub action_menu: Option<super::actions::ActionMenu>,
    pub action_message: Option<String>,
    pub filter: Filter,
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
        let frame = if pending && self.checking_sources {
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
        let mut failed = 0;
        let mut overdue = 0;
        for source in &self.snapshot.sources {
            let read = self.source_reads.get(&source.id);
            if source.stale.is_some() || read.is_some_and(|read| read.error.is_some()) {
                failed += 1;
            } else if read.is_some_and(ReadHealth::warns) {
                overdue += 1;
            }
        }
        failed = failed.max(usize::from(self.local_read.error.is_some()));
        let mut parts = Vec::new();
        if let Some(frame) = self.read_indicator {
            parts.push(format!("{} Reading sources", ["|", "/", "-", "\\"][frame]));
        }
        if failed > 0 {
            parts.push(format!("{failed} failed"));
        }
        if overdue > 0 {
            parts.push(format!("{overdue} overdue"));
        }
        if failed + overdue > 0 {
            parts.push("last good Work".into());
        } else if self.read_indicator.is_none() {
            parts.push(format!(
                "Source reads: {} healthy",
                self.snapshot.sources.len()
            ));
        }
        if !self.snapshot.errors.is_empty() || self.stale_error.is_some() {
            parts.push("read issues available".into());
        }
        parts.join(" · ")
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
            ];
        }
        if let Some(menu) = &self.action_menu {
            return if menu.current().remote() {
                vec![
                    "Esc Back",
                    "c Copy command",
                    "↑/↓ select",
                    "PgUp/PgDn scroll",
                ]
            } else {
                vec!["Esc Back", "Enter Choose", "c Copy command", "↑/↓ select"]
            };
        }
        self.project_navigation_controls()
    }

    pub fn project_navigation_controls(&self) -> Vec<&'static str> {
        let mut controls = if self.input_owner() == InputOwner::List {
            vec![
                "q quit",
                "Enter Open Work Item",
                "Tab/⇧Tab view",
                "? help",
                "↑/↓ select",
            ]
        } else if !self.show_details && self.terminal.is_some() && self.terminal_fits() {
            vec![
                "q quit",
                "Esc Back",
                "i Control",
                "f full-screen",
                "? help",
                "t reconnect",
                "o Observe",
            ]
        } else if self.page == ProjectPage::Worker {
            vec!["q quit", "Esc Back", "t terminal", "? help", "↑/↓ scroll"]
        } else {
            vec![
                "q quit",
                "Esc Back",
                "Enter Open worker",
                "? help",
                "↑/↓ workers",
                "t terminal",
            ]
        };
        if self.pane == Pane::Detail
            && self.show_details
            && self.page == ProjectPage::Overview
            && self
                .selected_key
                .as_ref()
                .and_then(|key| project::find_work(&self.snapshot.sources, key))
                .is_some_and(|(source, work)| !super::actions::offered(source, work).is_empty())
        {
            controls.insert(2, "a Actions");
        }
        if !self.snapshot.errors.is_empty()
            || self.source_warning_count() > 0
            || self.stale_error.is_some()
        {
            controls.insert(1, "e Read issues");
        }
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

    pub fn detail_values(&self) -> Vec<String> {
        if let Some(menu) = &self.action_menu {
            return menu.values();
        }
        let Some(key) = self.selected_key.as_ref() else {
            return vec!["No Work Item selected".into()];
        };
        if self.page == ProjectPage::Worker {
            return self
                .selected_worker
                .as_ref()
                .map(|worker| layout::project_detail_values(&self.snapshot.sources, worker))
                .unwrap_or_else(|| vec!["Worker unavailable; return and select a worker".into()]);
        }
        layout::project_overview_values(
            &self.snapshot.sources,
            key,
            self.selected_worker.as_ref(),
            self.size.0,
        )
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
        project::visible_keys(&self.snapshot.sources, self.filter)
    }
    pub fn all_work(&self) -> bool {
        self.all_work
    }
    pub fn scroll_bounds(&self) -> layout::ScrollBounds {
        if self.project_mode() {
            layout::project_scroll_bounds(
                &self.snapshot.sources,
                self.filter,
                &self.detail_values(),
                self.size,
                self.pane == Pane::Detail && self.show_details,
            )
        } else {
            layout::scroll_bounds(&self.snapshot, self.all_work, self.selected_id(), self.size)
        }
    }
}
