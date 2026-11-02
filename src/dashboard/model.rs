//! Share view state without coupling rendering to event handling.
use super::{
    layout,
    project::{self, Filter, RowKey},
    snapshot::DashboardSnapshot,
};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pane {
    List,
    Detail,
}
/// Track reads in memory; never serialize receipt times into Work or SSH data.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReadHealth {
    pub pending: bool,
    pub last_success: Option<chrono::DateTime<chrono::Utc>>,
    pub error: Option<String>,
}

pub struct App {
    pub local_read: ReadHealth,
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
}
impl App {
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
            )
        } else {
            layout::scroll_bounds(&self.snapshot, self.all_work, self.selected_id(), self.size)
        }
    }
}
