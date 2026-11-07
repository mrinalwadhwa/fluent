use super::layout;
use super::model::InputOwner;
pub(super) use super::model::{App, Pane};
use super::project::{self, Filter, RowKey};
use super::snapshot::DashboardSnapshot;
use crossterm::event::{KeyCode, KeyModifiers};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    None,
    Refresh,
    ToggleMouse(bool),
    OpenTerminal,
    ControlTerminal,
    FullscreenTerminal,
    ObserveTerminal,
    LiteralEscape,
}
impl App {
    pub fn new(snapshot: DashboardSnapshot) -> Self {
        let mut app = Self {
            snapshot,
            local_read: Default::default(),
            checking_sources: false,
            visible_read_since: None,
            read_indicator: None,
            source_reads: Default::default(),
            configuration_errors: Vec::new(),
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
            terminal: None,
            show_details: false,
        };
        if app.project_mode() {
            app.selected_key = project::initial_key(&app.snapshot.sources, app.filter);
            app.reconcile_project_selection();
            app.keep_selection_visible();
        } else {
            app.reconcile_selection(None);
        }
        app
    }

    pub fn begin_local_read(&mut self) {
        self.checking_sources |=
            self.local_read.last_success.is_none() && self.local_read.error.is_none();
        self.local_read.pending = true;
        for source in &self.snapshot.sources {
            if !source.id.starts_with("ssh:") {
                self.source_reads
                    .entry(source.id.clone())
                    .or_default()
                    .pending = true;
            }
        }
        self.dirty = true;
    }

    pub fn record_source_read(&mut self, source: &project::SourceSnapshot) {
        self.source_reads
            .entry(source.id.clone())
            .or_default()
            .complete(source.stale.as_deref());
        // Even an equal snapshot represents a new observation or error.
        self.dirty = true;
    }

    pub fn local_read_failed(&mut self, error: String) {
        self.local_read.complete(Some(&error));
        for (id, health) in &mut self.source_reads {
            if !id.starts_with("ssh:") && health.pending {
                health.complete(Some(&error));
            }
        }
        self.refresh_failed(error);
    }

    pub fn select_key(&mut self, key: RowKey) {
        self.selected_key = Some(key);
        self.reconcile_project_selection();
        self.keep_selection_visible();
        self.dirty = true;
    }

    pub fn should_quit(&self) -> bool {
        self.quit
    }
    pub fn take_dirty(&mut self) -> bool {
        std::mem::replace(&mut self.dirty, false)
    }
    pub fn set_terminal(&mut self, display: crate::terminal_view::Display) {
        self.terminal = Some(display);
        self.dirty = true;
    }
    pub fn terminal_fits(&self) -> bool {
        self.size.0 >= 60
            && self.size.1 >= 15
            && self.terminal.as_ref().is_some_and(|display| {
                layout::terminal_content(self.size, &display.identity).height > 0
            })
    }
    pub fn terminal_visible(&self) -> bool {
        self.project_mode()
            && !self.show_details
            && !self.help
            && !self.errors
            && self.terminal_fits()
            && (self.size.0 >= 100 || self.pane == Pane::Detail)
    }
    pub fn resize(&mut self, width: u16, height: u16) {
        self.size = (width, height);
        self.clamp_scrolls();
        if self.project_mode() && self.terminal.is_none() {
            self.keep_selection_visible();
        }
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
            // Keep the displayed generation selected until explicit navigation,
            // even when a source refresh removes its row.
            if self.terminal.is_none() {
                if self.selected_key.is_none() {
                    self.selected_key = project::initial_key(&self.snapshot.sources, self.filter);
                }
                self.reconcile_project_selection();
            }
        } else {
            self.reconcile_selection(prior);
        }
        self.clamp_scrolls();
        if self.project_mode() && self.terminal.is_none() {
            self.keep_selection_visible();
        }
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
        if self.terminal.is_some() && !self.help && !self.errors {
            match key {
                KeyCode::Char('i' | ']') if self.terminal_fits() => {
                    self.pane = Pane::Detail;
                    self.show_details = false;
                    self.dirty = true;
                    return if key == KeyCode::Char('i') {
                        Effect::ControlTerminal
                    } else {
                        Effect::LiteralEscape
                    };
                }
                KeyCode::Char('f') => return Effect::FullscreenTerminal,
                KeyCode::Char('o') => return Effect::ObserveTerminal,
                _ => {}
            }
        }
        if self.project_mode() && key == KeyCode::Char('t') && !self.help && !self.errors {
            self.pane = Pane::Detail;
            self.show_details = false;
            return Effect::OpenTerminal;
        }
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
                .saturating_sub(if self.project_mode() { 5 } else { 6 })
                .max(1);
            let max = if self.project_mode() {
                layout::project_error_max_scroll(&self.diagnostic_snapshot(), self.size)
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
            KeyCode::Right if self.project_mode() => {
                self.pane = Pane::Detail;
                self.show_details = true;
                self.detail_scroll = 0;
                self.dirty = true;
            }
            KeyCode::Tab | KeyCode::BackTab if self.project_mode() => {
                self.filter = if key == KeyCode::BackTab || modifiers.contains(KeyModifiers::SHIFT)
                {
                    self.filter.previous()
                } else {
                    self.filter.next()
                };
                self.reconcile_project_selection();
                self.detail_scroll = 0;
                self.clamp_scrolls();
                self.dirty = true;
            }
            KeyCode::Char('f') | KeyCode::Char('F') if self.project_mode() => {
                self.filter = self.filter.next();
                self.reconcile_project_selection();
                self.clamp_scrolls();
                self.dirty = true;
            }
            KeyCode::Enter if self.project_mode() && self.input_owner() == InputOwner::List => {
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
            KeyCode::Char('a') if self.project_mode() => {}
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
            KeyCode::Left if self.project_mode() => {
                self.pane = Pane::List;
                self.dirty = true;
            }
            KeyCode::Esc if self.pane == Pane::Detail => {
                self.pane = Pane::List;
                self.dirty = true;
            }
            KeyCode::Up | KeyCode::Char('k') if self.input_owner() == InputOwner::List => {
                self.move_selection(-1);
            }
            KeyCode::Down | KeyCode::Char('j') if self.input_owner() == InputOwner::List => {
                self.move_selection(1);
            }
            KeyCode::PageUp if self.input_owner() == InputOwner::List => {
                self.list_scroll = self
                    .list_scroll
                    .saturating_sub(self.scroll_bounds().list_height.max(1));
                self.dirty = true;
            }
            KeyCode::PageDown if self.input_owner() == InputOwner::List => {
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
                line += 1;
                for work in matching {
                    if &work.key == selected {
                        break;
                    }
                    line += 1;
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
            layout::project_error_max_scroll(&self.diagnostic_snapshot(), self.size)
        } else {
            layout::error_max_scroll(&self.snapshot, self.size)
        });
        let bounds = self.scroll_bounds();
        self.list_scroll = self.list_scroll.min(bounds.list_max_scroll);
        self.detail_scroll = self.detail_scroll.min(bounds.detail_max_scroll);
        self.keep_selection_visible();
    }
}
