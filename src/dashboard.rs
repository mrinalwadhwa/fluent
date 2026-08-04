mod app;
mod render;
mod snapshot;

use anyhow::Result;
use app::{App, Effect};
use crossterm::event::{self, Event as CEvent};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::{Terminal, backend::CrosstermBackend};
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::work_status;

const DATA_POLL_INTERVAL: Duration = Duration::from_secs(2);

struct TerminalSession {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
    mouse_enabled: bool,
}

impl TerminalSession {
    fn open() -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = crossterm::execute!(
            stdout,
            EnterAlternateScreen,
            crossterm::event::EnableMouseCapture
        ) {
            let _ = disable_raw_mode();
            return Err(error.into());
        }
        Ok(Self {
            terminal: Terminal::new(CrosstermBackend::new(stdout))?,
            mouse_enabled: true,
        })
    }

    fn set_mouse_enabled(&mut self, enabled: bool) -> Result<()> {
        if self.mouse_enabled != enabled {
            if enabled {
                crossterm::execute!(
                    self.terminal.backend_mut(),
                    crossterm::event::EnableMouseCapture
                )?;
            } else {
                crossterm::execute!(
                    self.terminal.backend_mut(),
                    crossterm::event::DisableMouseCapture
                )?;
            }
            self.mouse_enabled = enabled;
        }
        Ok(())
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = if self.mouse_enabled {
            crossterm::execute!(
                self.terminal.backend_mut(),
                crossterm::event::DisableMouseCapture,
                LeaveAlternateScreen
            )
        } else {
            crossterm::execute!(self.terminal.backend_mut(), LeaveAlternateScreen)
        };
        let _ = disable_raw_mode();
        let _ = self.terminal.show_cursor();
    }
}

/// Launch the read-only Work operator console.
pub fn run_dashboard(search_root: &Path) -> Result<()> {
    let status = work_status::load_work_status(search_root)?;
    let mut app = App::new(snapshot::DashboardSnapshot::from_status(status));
    let mut session = TerminalSession::open()?;
    let mut next_poll = Instant::now() + DATA_POLL_INTERVAL;

    loop {
        if app.take_dirty() {
            session.terminal.draw(|frame| render::draw(frame, &app))?;
        }
        let timeout = next_poll.saturating_duration_since(Instant::now());
        if event::poll(timeout)? {
            match event::read()? {
                CEvent::Key(key) => {
                    let effect = app.handle_key(key.code, key.modifiers);
                    if let Effect::ToggleMouse(enabled) = effect {
                        session.set_mouse_enabled(enabled)?;
                    }
                    if matches!(effect, Effect::Refresh) {
                        refresh(search_root, &mut app);
                        next_poll = Instant::now() + DATA_POLL_INTERVAL;
                    }
                }
                CEvent::Resize(width, height) => app.resize(width, height),
                _ => {}
            }
        }
        if Instant::now() >= next_poll {
            refresh(search_root, &mut app);
            next_poll = Instant::now() + DATA_POLL_INTERVAL;
        }
        if app.should_quit() {
            break;
        }
    }
    Ok(())
}

fn refresh(root: &Path, app: &mut App) {
    match work_status::load_work_status(root) {
        Ok(status) => app.refresh(snapshot::DashboardSnapshot::from_status(status)),
        Err(error) => app.refresh_failed(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::work_status::WorkItemStatus;
    use crossterm::event::{KeyCode, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

    fn row(id: &str, action: &str) -> WorkItemStatus {
        WorkItemStatus {
            id: id.into(),
            title: format!("{id} title"),
            attempt: "attempt [running]".into(),
            task: "write [running]".into(),
            review: "pending".into(),
            merge_candidate: "candidate".into(),
            merge: "pending".into(),
            action: action.into(),
            next_action: None,
            metrics: Default::default(),
            compatibility_warnings: vec![],
            release: None,
        }
    }
    fn app(rows: Vec<WorkItemStatus>) -> App {
        App::new(snapshot::DashboardSnapshot::from_status(
            crate::work_status::WorkStatus {
                rows,
                errors: vec![],
            },
        ))
    }
    fn text(app: &App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| render::draw(f, app)).unwrap();
        buffer_text(terminal.backend().buffer())
    }
    fn buffer_text(buffer: &Buffer) -> String {
        let area = buffer.area;
        (area.y..area.y + area.height)
            .map(|y| {
                (area.x..area.x + area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn dashboard_opens_on_current_work() {
        let app = app(vec![row("ready", "task-ready"), row("done", "complete")]);
        assert!(text(&app, 100, 24).contains("Ready (1)"));
        assert!(!text(&app, 100, 24).contains("done title"));
    }
    #[test]
    fn current_work_is_grouped_with_counts() {
        let app = app(vec![
            row("need", "needs-user"),
            row("run", "executing"),
            row("ready", "planned"),
        ]);
        let rendered = text(&app, 100, 24);
        assert!(rendered.contains("Needs you (1)"));
        assert!(rendered.contains("Running (1)"));
        assert!(rendered.contains("Ready (1)"));
    }
    #[test]
    fn empty_project_has_no_selection() {
        let app = app(vec![]);
        assert!(app.selected_id().is_none());
        assert!(text(&app, 100, 24).contains("No Work Items found"));
    }
    #[test]
    fn unknown_current_action_remains_visible() {
        let app = app(vec![row("new", "future-action")]);
        assert!(text(&app, 100, 24).contains("future-action"));
    }
    #[test]
    fn initial_selection_uses_first_non_empty_section() {
        let app = app(vec![row("ready", "planned"), row("need", "failed")]);
        assert_eq!(app.selected_id(), Some("need"));
    }
    #[test]
    fn wide_layout_shows_list_and_detail() {
        let app = app(vec![row("work", "task-ready")]);
        let rendered = text(&app, 100, 24);
        assert!(rendered.contains("Current Work"));
        assert!(rendered.contains("Work Item detail"));
        assert!(rendered.contains("Attempt: attempt [running]"));
    }
    #[test]
    fn detail_shows_canonical_next_action() {
        let mut status = row("work", "task-ready");
        status.next_action = Some("fluent attempt run work".into());
        let app = app(vec![status]);
        assert!(text(&app, 100, 24).contains("fluent attempt run work"));
    }
    #[test]
    fn detail_does_not_invent_next_action() {
        let app = app(vec![row("work", "executing")]);
        assert!(text(&app, 100, 24).contains("No operator action"));
    }
    #[test]
    fn selection_moves_across_groups_and_scrolls() {
        let mut app = app(vec![row("need", "failed"), row("run", "executing")]);
        app.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(app.selected_id(), Some("run"));
        assert_eq!(app.list_scroll, 1);
    }
    #[test]
    fn selection_survives_refresh_reorder_and_filter() {
        let mut app = app(vec![row("first", "planned"), row("selected", "planned")]);
        app.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        app.refresh(snapshot::DashboardSnapshot::from_status(
            crate::work_status::WorkStatus {
                rows: vec![row("selected", "executing"), row("first", "planned")],
                errors: vec![],
            },
        ));
        assert_eq!(app.selected_id(), Some("selected"));
        app.handle_key(KeyCode::Char('a'), KeyModifiers::NONE);
        assert_eq!(app.selected_id(), Some("selected"));
    }
    #[test]
    fn removed_selection_uses_nearest_remaining_row() {
        let mut app = app(vec![
            row("first", "planned"),
            row("selected", "planned"),
            row("last", "planned"),
        ]);
        app.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        app.refresh(snapshot::DashboardSnapshot::from_status(
            crate::work_status::WorkStatus {
                rows: vec![row("first", "planned"), row("last", "planned")],
                errors: vec![],
            },
        ));
        assert_eq!(app.selected_id(), Some("last"));
    }
    #[test]
    fn narrow_layout_switches_between_list_and_detail() {
        let mut app = app(vec![row("work", "planned")]);
        app.resize(80, 24);
        assert!(text(&app, 80, 24).contains("Work Items"));
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert!(text(&app, 80, 24).contains("Work Item detail"));
        app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(text(&app, 80, 24).contains("Work Items"));
    }
    #[test]
    fn undersized_terminal_shows_resize_message() {
        let app = app(vec![row("work", "planned")]);
        assert!(text(&app, 59, 14).contains("Resize terminal"));
    }
    #[test]
    fn overflow_detail_remains_navigable() {
        let mut status = row("work", "planned");
        status.compatibility_warnings = (0..30).map(|n| format!("warning {n}")).collect();
        let mut app = app(vec![status]);
        app.resize(80, 15);
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        app.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(app.detail_scroll, 1);
    }
}
