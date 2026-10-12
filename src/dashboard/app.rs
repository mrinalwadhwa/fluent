use super::layout;
use super::project::{self, Filter, RowKey};
use super::snapshot::DashboardSnapshot;
use crossterm::event::{KeyCode, KeyModifiers};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pane {
    List,
    Detail,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    None,
    Refresh,
    ToggleMouse(bool),
}
pub struct App {
    pub snapshot: DashboardSnapshot,
    all_work: bool,
    selected: Option<String>,
    pub selected_key: Option<RowKey>,
    pub filter: Filter,
    pub expanded: BTreeSet<RowKey>,
    selected_index: usize,
    pub pane: Pane,
    pub help: bool,
    pub errors: bool,
    pub error_scroll: u16,
    pub copy_mode: bool,
    pub list_scroll: u16,
    pub detail_scroll: u16,
    pub size: (u16, u16),
    dirty: bool,
    quit: bool,
    pub stale_error: Option<String>,
}
impl App {
    pub fn new(snapshot: DashboardSnapshot) -> Self {
        let mut app = Self {
            snapshot,
            all_work: false,
            selected: None,
            selected_key: None,
            filter: Filter::Current,
            expanded: BTreeSet::new(),
            selected_index: 0,
            pane: Pane::List,
            help: false,
            errors: false,
            error_scroll: 0,
            copy_mode: false,
            list_scroll: 0,
            detail_scroll: 0,
            size: (100, 24),
            dirty: true,
            quit: false,
            stale_error: None,
        };
        if app.project_mode() {
            app.selected_key = project::initial_key(&app.snapshot.sources, app.filter);
        } else {
            app.reconcile_selection(None);
        }
        app
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
    pub fn select_key(&mut self, key: RowKey) {
        self.selected_key = Some(key);
        self.reconcile_project_selection();
        self.keep_selection_visible();
        self.dirty = true;
    }
    pub fn all_work(&self) -> bool {
        self.all_work
    }
    pub fn should_quit(&self) -> bool {
        self.quit
    }
    pub fn take_dirty(&mut self) -> bool {
        std::mem::replace(&mut self.dirty, false)
    }
    pub fn resize(&mut self, width: u16, height: u16) {
        self.size = (width, height);
        self.clamp_scrolls();
        self.dirty = true;
    }
    pub fn refresh(&mut self, snapshot: DashboardSnapshot) {
        let was_stale = self.stale_error.is_some();
        if self.snapshot == snapshot && !was_stale {
            return;
        }
        let prior = self.selected.clone();
        self.snapshot = snapshot;
        self.stale_error = None;
        if self.project_mode() {
            self.reconcile_project_selection();
        } else {
            self.reconcile_selection(prior);
        }
        self.clamp_scrolls();
        self.dirty = true;
    }
    pub fn refresh_failed(&mut self, error: String) {
        self.stale_error = Some(error);
        self.clamp_scrolls();
        self.dirty = true;
    }
    fn reconcile_selection(&mut self, preferred: Option<String>) {
        let rows = self.snapshot.ordered_rows(self.all_work);
        if let Some(id) = preferred.and_then(|id| {
            rows.iter()
                .position(|row| row.status.id == id)
                .map(|i| (id, i))
        }) {
            self.selected = Some(id.0);
            self.selected_index = id.1;
        } else {
            self.selected_index = self.selected_index.min(rows.len().saturating_sub(1));
            self.selected = rows
                .get(self.selected_index)
                .map(|row| row.status.id.clone());
        }
    }
    fn reconcile_project_selection(&mut self) {
        let keys = self.project_keys();
        if let Some(index) = self
            .selected_key
            .as_ref()
            .and_then(|key| keys.iter().position(|candidate| candidate == key))
        {
            self.selected_index = index;
        } else {
            self.selected_index = self.selected_index.min(keys.len().saturating_sub(1));
            self.selected_key = keys.get(self.selected_index).cloned();
        }
    }
    fn move_selection(&mut self, delta: isize) {
        if self.project_mode() {
            let keys = self.project_keys();
            if keys.is_empty() {
                return;
            }
            self.selected_index =
                (self.selected_index as isize + delta).clamp(0, keys.len() as isize - 1) as usize;
            self.selected_key = keys.get(self.selected_index).cloned();
            self.keep_selection_visible();
            self.detail_scroll = 0;
            self.dirty = true;
            return;
        }
        let rows = self.snapshot.ordered_rows(self.all_work);
        if rows.is_empty() {
            return;
        }
        self.selected_index =
            (self.selected_index as isize + delta).clamp(0, rows.len() as isize - 1) as usize;
        self.selected = Some(rows[self.selected_index].status.id.clone());
        self.keep_selection_visible();
        self.detail_scroll = 0;
        self.dirty = true;
    }
    pub fn handle_key(&mut self, key: KeyCode, modifiers: KeyModifiers) -> Effect {
        if (modifiers.contains(KeyModifiers::CONTROL) && key == KeyCode::Char('c'))
            || key == KeyCode::Char('q')
        {
            self.quit = true;
            self.dirty = true;
            return Effect::None;
        }
        if key == KeyCode::Char('?') || key == KeyCode::Esc && self.help {
            self.help = !self.help;
            self.dirty = true;
            return Effect::None;
        }
        if self.help {
            return Effect::None;
        }
        if key == KeyCode::Char('e') || key == KeyCode::Esc && self.errors {
            self.errors = !self.errors;
            self.dirty = true;
            return Effect::None;
        }
        if self.errors {
            let page = self
                .size
                .1
                .saturating_sub(if self.project_mode() { 8 } else { 6 })
                .max(1);
            let max = if self.project_mode() {
                layout::project_error_max_scroll(&self.snapshot, self.size)
            } else {
                layout::error_max_scroll(&self.snapshot, self.size)
            };
            self.error_scroll = match key {
                KeyCode::Up | KeyCode::Char('k') => self.error_scroll.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => self.error_scroll.saturating_add(1).min(max),
                KeyCode::PageUp => self.error_scroll.saturating_sub(page),
                KeyCode::PageDown => self.error_scroll.saturating_add(page).min(max),
                KeyCode::Char('r') => return Effect::Refresh,
                _ => self.error_scroll,
            };
            self.dirty = true;
            return Effect::None;
        }
        match key {
            KeyCode::Tab if self.project_mode() && self.size.0 >= 60 && self.size.0 < 100 => {
                self.pane = if self.pane == Pane::List {
                    Pane::Detail
                } else {
                    Pane::List
                };
                self.dirty = true;
            }
            KeyCode::Char('f') if self.project_mode() => {
                self.filter = self.filter.next();
                self.reconcile_project_selection();
                self.clamp_scrolls();
                self.dirty = true;
            }
            KeyCode::Enter if self.project_mode() && self.pane == Pane::List => {
                if let Some(selected) = self.selected_key.clone() {
                    if selected.task_id.is_none() {
                        if !self.expanded.remove(&selected) {
                            self.expanded.insert(selected);
                        }
                        self.reconcile_project_selection();
                        self.clamp_scrolls();
                    } else if self.size.0 < 100 {
                        self.pane = Pane::Detail;
                    }
                }
                self.dirty = true;
            }
            KeyCode::Char('a') if self.project_mode() => {
                self.filter = if self.filter == Filter::Category(project::Category::History) {
                    Filter::Current
                } else {
                    Filter::Category(project::Category::History)
                };
                self.reconcile_project_selection();
                self.clamp_scrolls();
                self.dirty = true;
            }
            KeyCode::Char('a') => {
                self.all_work = !self.all_work;
                self.reconcile_selection(self.selected.clone());
                self.clamp_scrolls();
                self.dirty = true;
            }
            KeyCode::Char('r') => return Effect::Refresh,
            KeyCode::Char('c') => {
                self.copy_mode = !self.copy_mode;
                self.dirty = true;
                return Effect::ToggleMouse(!self.copy_mode);
            }
            KeyCode::Enter if self.size.0 < 100 && self.size.0 >= 60 => {
                self.pane = Pane::Detail;
                self.dirty = true;
            }
            KeyCode::Esc if self.pane == Pane::Detail => {
                self.pane = Pane::List;
                self.dirty = true;
            }
            KeyCode::Up | KeyCode::Char('k') if self.pane == Pane::List => {
                self.move_selection(-1);
            }
            KeyCode::Down | KeyCode::Char('j') if self.pane == Pane::List => {
                self.move_selection(1);
            }
            KeyCode::PageUp if self.pane == Pane::List => {
                self.list_scroll = self
                    .list_scroll
                    .saturating_sub(self.scroll_bounds().list_height.max(1));
                self.dirty = true;
            }
            KeyCode::PageDown if self.pane == Pane::List => {
                self.list_scroll = self
                    .list_scroll
                    .saturating_add(self.scroll_bounds().list_height.max(1))
                    .min(self.scroll_bounds().list_max_scroll);
                self.dirty = true;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.detail_scroll = self.detail_scroll.saturating_sub(1);
                self.dirty = true;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.detail_scroll = self
                    .detail_scroll
                    .saturating_add(1)
                    .min(self.scroll_bounds().detail_max_scroll);
                self.dirty = true;
            }
            _ => {}
        }
        Effect::None
    }

    fn keep_selection_visible(&mut self) {
        if self.project_mode() {
            let bounds = self.scroll_bounds();
            let Some(selected) = &self.selected_key else {
                return;
            };
            let mut line = 0u16;
            for source in &self.snapshot.sources {
                let matching = source
                    .work
                    .iter()
                    .filter(|work| self.filter.matches(work.category))
                    .collect::<Vec<_>>();
                if matching.is_empty() {
                    continue;
                }
                line += 1;
                for work in matching {
                    if &work.key == selected {
                        break;
                    }
                    line += 1;
                    if self.expanded.contains(&work.key) {
                        if let Some(index) = work
                            .workers
                            .iter()
                            .position(|worker| &worker.key == selected)
                        {
                            line += index as u16;
                            break;
                        }
                        line += work.workers.len() as u16;
                    }
                }
                if selected.source_id == source.id {
                    break;
                }
            }
            if line < self.list_scroll {
                self.list_scroll = line;
            } else if bounds.list_height > 0 && line >= self.list_scroll + bounds.list_height {
                self.list_scroll = line + 1 - bounds.list_height;
            }
            self.list_scroll = self.list_scroll.min(bounds.list_max_scroll);
            return;
        }
        let bounds = self.scroll_bounds();
        let Some(line) = self
            .selected
            .as_deref()
            .and_then(|id| self.snapshot.selected_line(self.all_work, id))
            .map(|line| line as u16)
        else {
            return;
        };
        if line < self.list_scroll {
            self.list_scroll = line;
        } else if bounds.list_height > 0 && line >= self.list_scroll + bounds.list_height {
            self.list_scroll = line + 1 - bounds.list_height;
        }
        self.list_scroll = self.list_scroll.min(bounds.list_max_scroll);
    }

    fn clamp_scrolls(&mut self) {
        self.error_scroll = self.error_scroll.min(if self.project_mode() {
            layout::project_error_max_scroll(&self.snapshot, self.size)
        } else {
            layout::error_max_scroll(&self.snapshot, self.size)
        });
        let bounds = self.scroll_bounds();
        self.list_scroll = self.list_scroll.min(bounds.list_max_scroll);
        self.detail_scroll = self.detail_scroll.min(bounds.detail_max_scroll);
        self.keep_selection_visible();
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
