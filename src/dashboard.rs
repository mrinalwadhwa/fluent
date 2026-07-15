use anyhow::Result;
use crossterm::event::{self, Event as CEvent, KeyCode, KeyModifiers};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::work_status::{self, WorkStatus};

const RENDER_INTERVAL: Duration = Duration::from_millis(75);
const DATA_POLL_INTERVAL: Duration = Duration::from_millis(2000);

/// Top-level dashboard app state.
struct App {
    work_status: WorkStatus,
    search_root: std::path::PathBuf,
    should_quit: bool,
    /// Monotonically increasing counter, incremented each render frame.
    tick: u64,
    /// When true, mouse capture is disabled so the user can select text.
    copy_mode: bool,
}

impl App {
    fn new(search_root: &Path) -> Result<Self> {
        let work_status = work_status::load_work_status(search_root)?;

        Ok(Self {
            work_status,
            search_root: search_root.to_path_buf(),
            should_quit: false,
            tick: 0,
            copy_mode: false,
        })
    }

    fn poll(&mut self) {
        if let Ok(work_status) = work_status::load_work_status(&self.search_root) {
            self.work_status = work_status;
        }
    }

    /// Toggle copy mode and return the new state.
    /// Caller is responsible for issuing the terminal mouse capture command.
    fn toggle_copy_mode(&mut self) -> bool {
        self.copy_mode = !self.copy_mode;
        self.copy_mode
    }
}

/// Launch the dashboard TUI.
pub fn run_dashboard(search_root: &Path) -> Result<()> {
    let mut app = App::new(search_root)?;

    // Set up terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(
        stdout,
        EnterAlternateScreen,
        crossterm::event::EnableMouseCapture
    )?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_event_loop(&mut terminal, &mut app);

    // Restore terminal — only re-disable mouse capture if copy mode hasn't
    // already done so, to avoid sending a redundant escape sequence.
    disable_raw_mode()?;
    if app.copy_mode {
        crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    } else {
        crossterm::execute!(
            terminal.backend_mut(),
            LeaveAlternateScreen,
            crossterm::event::DisableMouseCapture
        )?;
    }
    terminal.show_cursor()?;

    result
}

fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
) -> Result<()> {
    let mut last_data_poll = Instant::now();
    let mut last_render = Instant::now();

    loop {
        // Render frequently enough for the UI to feel responsive.
        if last_render.elapsed() >= RENDER_INTERVAL {
            terminal.draw(|f| draw_ui(f, app))?;
            last_render = Instant::now();
        }

        // Sleep only for the remaining render budget.
        let timeout = RENDER_INTERVAL
            .checked_sub(last_render.elapsed())
            .unwrap_or(Duration::ZERO);

        if event::poll(timeout)? {
            match event::read()? {
                CEvent::Key(key) => match (key.modifiers, key.code) {
                    (KeyModifiers::CONTROL, KeyCode::Char('c')) | (_, KeyCode::Char('q')) => {
                        app.should_quit = true;
                    }
                    (_, KeyCode::Char('c')) => {
                        if app.toggle_copy_mode() {
                            crossterm::execute!(
                                terminal.backend_mut(),
                                crossterm::event::DisableMouseCapture
                            )?;
                        } else {
                            crossterm::execute!(
                                terminal.backend_mut(),
                                crossterm::event::EnableMouseCapture
                            )?;
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
        }

        // Periodic poll for new data at a slower cadence
        if last_data_poll.elapsed() >= DATA_POLL_INTERVAL {
            app.poll();
            last_data_poll = Instant::now();
        }

        if app.should_quit {
            app.poll();
            terminal.clear()?;
            terminal.draw(|f| draw_ui(f, app))?;
            break;
        }
    }

    Ok(())
}

fn draw_ui(f: &mut ratatui::Frame, app: &mut App) {
    let size = f.area();
    app.tick += 1;

    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // header
            Constraint::Min(6),    // work items
            Constraint::Length(1), // help bar
        ])
        .split(size);

    draw_work_header(f, main_chunks[0], app);
    draw_work_items(f, main_chunks[1], &app.work_status);
    draw_help_bar(f, main_chunks[2], app.copy_mode);
}

fn draw_work_header(f: &mut ratatui::Frame, area: Rect, app: &App) {
    let rows = app.work_status.rows.len();
    let errors = app.work_status.errors.len();
    let actionable = app
        .work_status
        .rows
        .iter()
        .filter(|row| {
            matches!(
                row.action.as_str(),
                "needs-user" | "merge-ready" | "task-ready" | "merge-failed" | "failed"
            )
        })
        .count();
    let header = Paragraph::new(Line::from(vec![
        Span::styled("Work Items: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{rows}"),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled("Actionable: ", Style::default().fg(Color::DarkGray)),
        Span::styled(format!("{actionable}"), Style::default().fg(Color::Yellow)),
        Span::raw("  "),
        Span::styled("Errors: ", Style::default().fg(Color::DarkGray)),
        Span::styled(format!("{errors}"), Style::default().fg(Color::Red)),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Fluent Dashboard "),
    );

    f.render_widget(header, area);
}

fn draw_work_items(f: &mut ratatui::Frame, area: Rect, status: &WorkStatus) {
    let lines: Vec<Line> = work_status::format_work_dashboard_lines(status)
        .into_iter()
        .map(|line| {
            let style = if line.contains("[needs-user]") || line.contains("needs-user") {
                Style::default().fg(Color::Yellow)
            } else if line.contains("[merge-ready]") || line.contains("merge-ready") {
                Style::default().fg(Color::Green)
            } else if line.contains("failed") {
                Style::default().fg(Color::Red)
            } else if line.starts_with("  ") {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default().fg(Color::White)
            };
            Line::from(Span::styled(line, style))
        })
        .collect();
    let paragraph =
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" Work Items "));
    f.render_widget(paragraph, area);
}

fn draw_help_bar(f: &mut ratatui::Frame, area: Rect, copy_mode: bool) {
    let mut spans = vec![
        Span::styled(" q", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" quit  "),
    ];
    if copy_mode {
        spans.push(Span::styled(
            "[COPY MODE] ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }
    spans.extend([
        Span::styled("c", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" copy"),
    ]);
    let help = Paragraph::new(Line::from(spans)).style(Style::default().fg(Color::DarkGray));

    f.render_widget(help, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::work_model::{WorkItem, WorkModelStore};
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;

    /// Extract all text content from a buffer (concatenated lines).
    fn buffer_text(buf: &Buffer) -> String {
        let area = buf.area;
        let mut text = String::new();
        for y in area.y..area.y + area.height {
            for x in area.x..area.x + area.width {
                let cell = &buf[(x, y)];
                text.push_str(cell.symbol());
            }
            text.push('\n');
        }
        text
    }

    fn make_app(work_status: WorkStatus) -> App {
        App {
            work_status,
            search_root: std::path::PathBuf::from("/tmp"),
            should_quit: false,
            tick: 0,
            copy_mode: false,
        }
    }

    fn render_app_text(app: &mut App) -> String {
        let backend = TestBackend::new(100, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw_ui(f, app);
            })
            .unwrap();
        buffer_text(terminal.backend().buffer())
    }

    fn write_work_item(project_root: &Path, id: &str, title: &str) {
        let mut item = WorkItem {
            id: id.to_string(),
            title: title.to_string(),
            planning_context: None,
            instructions: None,
            abandonment: None,
            post_merge_review_fix_depth: None,
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
        };
        item.add_initial_attempt(format!("{id}-attempt")).unwrap();
        WorkModelStore::new(project_root)
            .write_work_item(&item)
            .unwrap();
    }

    #[test]
    fn test_work_view_renders_work_items() {
        let mut app = make_app(WorkStatus {
            rows: vec![work_status::WorkItemStatus {
                id: "work-1".to_string(),
                title: "Build status view".to_string(),
                attempt: "attempt-1 [planned]".to_string(),
                task: "write:attempt-1-write-1 [planned]".to_string(),
                review: "-".to_string(),
                merge_candidate: "-".to_string(),
                merge: "-".to_string(),
                action: "task-ready".to_string(),
            }],
            errors: Vec::new(),
        });

        let text = render_app_text(&mut app);

        assert!(text.contains("Work Items"));
        assert!(text.contains("work-1"));
        assert!(text.contains("Build status view"));
        assert!(text.contains("task-ready"));
        assert!(text.contains("Attempt: attempt-1 [planned]"));
    }

    #[test]
    fn test_work_view_renders_empty_state() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut app = App::new(tmp.path()).unwrap();

        let text = render_app_text(&mut app);

        assert!(text.contains("No Work Items found"));
    }

    #[test]
    fn test_work_view_counts_errors() {
        let mut app = make_app(WorkStatus {
            rows: vec![work_status::WorkItemStatus {
                id: "work-1".to_string(),
                title: "Build status view".to_string(),
                attempt: "attempt-1 [planned]".to_string(),
                task: "write:attempt-1-write-1 [planned]".to_string(),
                review: "-".to_string(),
                merge_candidate: "-".to_string(),
                merge: "-".to_string(),
                action: "task-ready".to_string(),
            }],
            errors: vec!["invalid Work Item file .fluent/work/items/broken-work.json".to_string()],
        });

        let text = render_app_text(&mut app);

        assert!(text.contains("Work Items: 1"));
        assert!(text.contains("Actionable: 1"));
        assert!(text.contains("Errors: 1"));
        assert!(text.contains("Work Item read errors"));
        assert!(text.contains(".fluent/work/items/broken-work.json"));
    }

    #[test]
    fn test_app_poll_refreshes_work_items() {
        let tmp = tempfile::TempDir::new().unwrap();
        write_work_item(tmp.path(), "work-before", "Before poll");
        let mut app = App::new(tmp.path()).unwrap();

        assert_eq!(app.work_status.rows.len(), 1);
        assert_eq!(app.work_status.rows[0].id, "work-before");

        write_work_item(tmp.path(), "work-after", "After poll");
        app.poll();

        let ids: Vec<&str> = app
            .work_status
            .rows
            .iter()
            .map(|row| row.id.as_str())
            .collect();
        assert_eq!(ids, vec!["work-after", "work-before"]);
        let text = render_app_text(&mut app);
        assert!(text.contains("work-after - After poll"));
    }

    // --- Copy mode help bar test ---

    #[test]
    fn test_help_bar_shows_copy_key() {
        let backend = TestBackend::new(80, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw_help_bar(f, f.area(), false);
            })
            .unwrap();
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("copy"));
        assert!(!text.contains("[COPY MODE]"));
    }

    #[test]
    fn test_help_bar_shows_copy_mode_indicator() {
        let backend = TestBackend::new(80, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw_help_bar(f, f.area(), true);
            })
            .unwrap();
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("[COPY MODE]"));
    }

    // --- Copy mode toggle test ---

    #[test]
    fn test_toggle_copy_mode() {
        let mut app = make_app(WorkStatus {
            rows: Vec::new(),
            errors: Vec::new(),
        });
        assert!(!app.copy_mode);

        // First toggle enables copy mode
        assert!(app.toggle_copy_mode());
        assert!(app.copy_mode);

        // Second toggle disables copy mode
        assert!(!app.toggle_copy_mode());
        assert!(!app.copy_mode);
    }
}
