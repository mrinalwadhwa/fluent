use super::snapshot::DashboardSnapshot;
use crossterm::event::{KeyCode, KeyModifiers};

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
    selected_index: usize,
    pub pane: Pane,
    pub help: bool,
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
            selected_index: 0,
            pane: Pane::List,
            help: false,
            copy_mode: false,
            list_scroll: 0,
            detail_scroll: 0,
            size: (100, 24),
            dirty: true,
            quit: false,
            stale_error: None,
        };
        app.reconcile_selection(None);
        app
    }
    pub fn selected_id(&self) -> Option<&str> {
        self.selected.as_deref()
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
        self.dirty = true;
    }
    pub fn refresh(&mut self, snapshot: DashboardSnapshot) {
        let prior = self.selected.clone();
        self.snapshot = snapshot;
        self.stale_error = None;
        self.reconcile_selection(prior);
        self.dirty = true;
    }
    pub fn refresh_failed(&mut self, error: String) {
        self.stale_error = Some(error);
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
    fn move_selection(&mut self, delta: isize) {
        let rows = self.snapshot.ordered_rows(self.all_work);
        if rows.is_empty() {
            return;
        }
        self.selected_index =
            (self.selected_index as isize + delta).clamp(0, rows.len() as isize - 1) as usize;
        self.selected = Some(rows[self.selected_index].status.id.clone());
        self.list_scroll = self.selected_index as u16;
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
        match key {
            KeyCode::Char('a') => {
                self.all_work = !self.all_work;
                self.reconcile_selection(self.selected.clone());
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
            KeyCode::Up | KeyCode::Char('k') if self.pane == Pane::List => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') if self.pane == Pane::List => self.move_selection(1),
            KeyCode::Up | KeyCode::Char('k') => {
                self.detail_scroll = self.detail_scroll.saturating_sub(1);
                self.dirty = true;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.detail_scroll = self.detail_scroll.saturating_add(1);
                self.dirty = true;
            }
            _ => {}
        }
        Effect::None
    }
}
