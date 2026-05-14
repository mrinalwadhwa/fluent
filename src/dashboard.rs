use anyhow::Result;
use crossterm::event::{self, Event as CEvent, KeyCode, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Tabs};
use ratatui::Terminal;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::run::{self, Run, RunStatus};
use crate::transcript::{self, Event, TranscriptReader};

const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// State for a single run's dashboard view.
struct RunView {
    run: Run,
    /// The directory where sessions/transcripts live — the worktree's run dir
    /// if a worktree exists, otherwise the source run dir.
    live_dir: PathBuf,
    events: Vec<Event>,
    readers: Vec<TranscriptReader>,
    last_session: u32,
    scroll_offset: usize,
    auto_scroll: bool,
}

impl RunView {
    fn new(run: Run) -> Self {
        // Resolve worktree path for live session data
        let live_dir = run.worktree_run_dir().unwrap_or_else(|| run.dir.clone());
        let mut view = Self {
            run,
            live_dir,
            events: Vec::new(),
            readers: Vec::new(),
            last_session: 0,
            scroll_offset: 0,
            auto_scroll: true,
        };
        view.discover_sessions();
        view.poll();
        view
    }

    fn discover_sessions(&mut self) {
        let transcripts = transcript::list_transcripts(&self.live_dir);
        for (num, path) in transcripts {
            if num > self.last_session {
                self.readers.push(TranscriptReader::new(path));
                self.last_session = num;
            }
        }
    }

    fn poll(&mut self) {
        // Check for new sessions
        self.discover_sessions();

        // Read new events from all transcript readers
        for reader in &mut self.readers {
            let new_events = reader.read_new();
            self.events.extend(new_events);
        }

        if self.auto_scroll {
            self.scroll_to_bottom();
        }
    }

    fn scroll_to_bottom(&mut self) {
        let visible = self.visible_lines();
        if visible.len() > 0 {
            self.scroll_offset = visible.len().saturating_sub(1);
        }
    }

    fn visible_lines(&self) -> Vec<String> {
        self.events
            .iter()
            .filter_map(|e| {
                let s = e.summary();
                if s.is_empty() {
                    None
                } else {
                    Some(s)
                }
            })
            .collect()
    }
}

/// Top-level dashboard app state.
struct App {
    runs: Vec<RunView>,
    selected_run: usize,
    search_root: PathBuf,
    should_quit: bool,
}

impl App {
    fn new(search_root: &Path, target_run_id: Option<&str>) -> Result<Self> {
        let all_runs = run::list_runs(search_root)?;

        let views: Vec<RunView> = all_runs
            .into_iter()
            .map(|r| RunView::new(r))
            .collect();

        // Find the index of the target run, or pick the first active one
        let selected = if let Some(id) = target_run_id {
            views.iter().position(|v| v.run.id == id).unwrap_or(0)
        } else {
            views
                .iter()
                .position(|v| {
                    matches!(
                        v.run.status().unwrap_or(RunStatus::Unknown("-".into())),
                        RunStatus::Executing | RunStatus::Planned
                    )
                })
                .unwrap_or(0)
        };

        if views.is_empty() {
            anyhow::bail!("No runs found in {}", search_root.display());
        }

        Ok(Self {
            runs: views,
            selected_run: selected,
            search_root: search_root.to_path_buf(),
            should_quit: false,
        })
    }

    fn poll(&mut self) {
        // Check for new runs
        if let Ok(all_runs) = run::list_runs(&self.search_root) {
            let existing_ids: Vec<String> =
                self.runs.iter().map(|v| v.run.id.clone()).collect();
            for r in all_runs {
                if !existing_ids.contains(&r.id) {
                    self.runs.push(RunView::new(r));
                }
            }
        }

        for view in &mut self.runs {
            view.poll();
        }
    }

    fn current_view(&self) -> &RunView {
        &self.runs[self.selected_run]
    }

    fn current_view_mut(&mut self) -> &mut RunView {
        &mut self.runs[self.selected_run]
    }
}

/// Launch the dashboard TUI.
pub fn run_dashboard(search_root: &Path, run_id: Option<&str>) -> Result<()> {
    let mut app = App::new(search_root, run_id)?;

    // Set up terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_event_loop(&mut terminal, &mut app);

    // Restore terminal
    disable_raw_mode()?;
    crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
) -> Result<()> {
    let mut last_poll = Instant::now();

    loop {
        terminal.draw(|f| draw_ui(f, app))?;

        // Poll for events with timeout
        let timeout = POLL_INTERVAL
            .checked_sub(last_poll.elapsed())
            .unwrap_or(Duration::ZERO);

        if event::poll(timeout)? {
            if let CEvent::Key(key) = event::read()? {
                match (key.modifiers, key.code) {
                    (KeyModifiers::CONTROL, KeyCode::Char('c'))
                    | (_, KeyCode::Char('q')) => {
                        app.should_quit = true;
                    }
                    (_, KeyCode::Tab) | (_, KeyCode::Right) => {
                        if !app.runs.is_empty() {
                            app.selected_run =
                                (app.selected_run + 1) % app.runs.len();
                        }
                    }
                    (_, KeyCode::BackTab) | (_, KeyCode::Left) => {
                        if !app.runs.is_empty() {
                            app.selected_run = if app.selected_run == 0 {
                                app.runs.len() - 1
                            } else {
                                app.selected_run - 1
                            };
                        }
                    }
                    (_, KeyCode::Up) | (_, KeyCode::Char('k')) => {
                        let view = app.current_view_mut();
                        view.auto_scroll = false;
                        view.scroll_offset =
                            view.scroll_offset.saturating_sub(1);
                    }
                    (_, KeyCode::Down) | (_, KeyCode::Char('j')) => {
                        let view = app.current_view_mut();
                        let max = view.visible_lines().len().saturating_sub(1);
                        view.scroll_offset =
                            (view.scroll_offset + 1).min(max);
                    }
                    (_, KeyCode::Char('G')) | (_, KeyCode::End) => {
                        let view = app.current_view_mut();
                        view.auto_scroll = true;
                        view.scroll_to_bottom();
                    }
                    (_, KeyCode::Char('g')) | (_, KeyCode::Home) => {
                        let view = app.current_view_mut();
                        view.auto_scroll = false;
                        view.scroll_offset = 0;
                    }
                    (_, KeyCode::PageUp) => {
                        let view = app.current_view_mut();
                        view.auto_scroll = false;
                        view.scroll_offset =
                            view.scroll_offset.saturating_sub(20);
                    }
                    (_, KeyCode::PageDown) => {
                        let view = app.current_view_mut();
                        let max = view.visible_lines().len().saturating_sub(1);
                        view.scroll_offset =
                            (view.scroll_offset + 20).min(max);
                    }
                    _ => {}
                }
            }
        }

        if app.should_quit {
            break;
        }

        // Periodic poll for new data
        if last_poll.elapsed() >= POLL_INTERVAL {
            app.poll();
            last_poll = Instant::now();
        }
    }

    Ok(())
}

fn draw_ui(f: &mut ratatui::Frame, app: &App) {
    let size = f.area();

    // Check if we need a reviewer panel at the bottom
    let view = app.current_view();
    let has_reviewers = view.live_dir.join("reviews").is_dir()
        && std::fs::read_dir(view.live_dir.join("reviews"))
            .map(|mut e| e.next().is_some())
            .unwrap_or(false);

    let main_chunks = if has_reviewers {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),  // header
                Constraint::Length(3),  // run tabs
                Constraint::Min(10),   // activity feed
                Constraint::Length(7), // reviewer panel (5 reviewers + border)
                Constraint::Length(1),  // help bar
            ])
            .split(size)
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),  // header
                Constraint::Length(3),  // run tabs
                Constraint::Min(10),   // activity feed
                Constraint::Length(1),  // help bar
            ])
            .split(size)
    };

    draw_header(f, main_chunks[0], view);
    draw_run_tabs(f, main_chunks[1], app);
    draw_activity_feed(f, main_chunks[2], view);

    if has_reviewers {
        draw_reviewer_panel(f, main_chunks[3], view);
        draw_help_bar(f, main_chunks[4]);
    } else {
        draw_help_bar(f, main_chunks[3]);
    }
}

fn draw_header(f: &mut ratatui::Frame, area: Rect, view: &RunView) {
    let status = view
        .run
        .status()
        .map(|s| s.to_string())
        .unwrap_or_else(|_| "?".into());

    let status_color = match status.as_str() {
        "executing" => Color::Green,
        "complete" => Color::Blue,
        "failed" => Color::Red,
        "needs-user" => Color::Yellow,
        "rate-limited" => Color::Magenta,
        "planned" => Color::Cyan,
        _ => Color::White,
    };

    let session_count = view.last_session;
    let event_count = view.events.len();

    let header = Paragraph::new(Line::from(vec![
        Span::styled("Run: ", Style::default().fg(Color::DarkGray)),
        Span::styled(&view.run.id, Style::default().add_modifier(Modifier::BOLD)),
        Span::raw("  "),
        Span::styled("Status: ", Style::default().fg(Color::DarkGray)),
        Span::styled(status, Style::default().fg(status_color).add_modifier(Modifier::BOLD)),
        Span::raw("  "),
        Span::styled("Session: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{session_count}"),
            Style::default(),
        ),
        Span::raw("  "),
        Span::styled("Events: ", Style::default().fg(Color::DarkGray)),
        Span::styled(format!("{event_count}"), Style::default()),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Factory Dashboard "),
    );

    f.render_widget(header, area);
}

fn draw_run_tabs(f: &mut ratatui::Frame, area: Rect, app: &App) {
    let titles: Vec<Line> = app
        .runs
        .iter()
        .map(|v| {
            let status = v
                .run
                .status()
                .map(|s| s.to_string())
                .unwrap_or_else(|_| "?".into());
            let color = match status.as_str() {
                "executing" => Color::Green,
                "complete" => Color::Blue,
                "failed" => Color::Red,
                "needs-user" => Color::Yellow,
                _ => Color::White,
            };
            Line::from(Span::styled(
                format!(" {} [{}] ", v.run.id, status),
                Style::default().fg(color),
            ))
        })
        .collect();

    let tabs = Tabs::new(titles)
        .block(Block::default().borders(Borders::ALL).title(" Runs "))
        .select(app.selected_run)
        .highlight_style(
            Style::default()
                .add_modifier(Modifier::BOLD)
                .add_modifier(Modifier::REVERSED),
        );

    f.render_widget(tabs, area);
}

fn draw_activity_feed(f: &mut ratatui::Frame, area: Rect, view: &RunView) {
    let lines = view.visible_lines();
    let visible_height = area.height.saturating_sub(2) as usize; // account for borders

    // Calculate visible window
    let total = lines.len();
    let start = if view.auto_scroll {
        total.saturating_sub(visible_height)
    } else {
        view.scroll_offset.min(total.saturating_sub(visible_height))
    };
    let end = (start + visible_height).min(total);

    let items: Vec<ListItem> = lines[start..end]
        .iter()
        .map(|line| {
            let style = style_for_line(line);
            ListItem::new(Line::from(Span::styled(line.as_str(), style)))
        })
        .collect();

    let scroll_indicator = if total > visible_height {
        let pct = if total == 0 {
            100
        } else {
            ((end as f64 / total as f64) * 100.0) as usize
        };
        format!(" {pct}% ")
    } else {
        String::new()
    };

    let title = format!(
        " Activity [{}/{}]{} ",
        end.min(total),
        total,
        if view.auto_scroll {
            " [auto-scroll]"
        } else {
            ""
        }
    );

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title(title)
            .title_bottom(scroll_indicator),
    );

    f.render_widget(list, area);
}

fn style_for_line(line: &str) -> Style {
    if line.starts_with('[') {
        // Tool use
        if line.starts_with("[Bash]") {
            Style::default().fg(Color::Yellow)
        } else if line.starts_with("[Read]") {
            Style::default().fg(Color::Cyan)
        } else if line.starts_with("[Edit]") || line.starts_with("[Write]") {
            Style::default().fg(Color::Green)
        } else if line.starts_with("[Grep]") || line.starts_with("[Glob]") {
            Style::default().fg(Color::Magenta)
        } else if line.starts_with("[Agent]") {
            Style::default().fg(Color::Blue)
        } else {
            Style::default().fg(Color::DarkGray)
        }
    } else if line.starts_with("Session started") || line.starts_with("Session complete") {
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else if line == "thinking..." {
        Style::default().fg(Color::DarkGray)
    } else if line.starts_with("rate limit") {
        Style::default().fg(Color::Magenta)
    } else {
        Style::default().fg(Color::White)
    }
}

fn draw_reviewer_panel(f: &mut ratatui::Frame, area: Rect, view: &RunView) {
    let reviews_dir = view.live_dir.join("reviews");
    let mut reviewer_lines = Vec::new();

    if reviews_dir.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&reviews_dir) {
            let mut entries: Vec<_> = entries.flatten().collect();
            entries.sort_by_key(|e| e.file_name());
            for entry in entries {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with("review-") && name.ends_with(".md") {
                    let reviewer = name
                        .strip_prefix("review-")
                        .and_then(|s| s.strip_suffix(".md"))
                        .unwrap_or(&name);

                    // Try to read verdict from the file
                    let verdict = std::fs::read_to_string(entry.path())
                        .ok()
                        .and_then(|content| {
                            content
                                .lines()
                                .find(|l| l.to_lowercase().contains("verdict"))
                                .map(|l| l.to_string())
                        })
                        .unwrap_or_else(|| "in progress".to_string());

                    let (symbol, color) = if verdict.to_lowercase().contains("pass") {
                        ("PASS", Color::Green)
                    } else if verdict.to_lowercase().contains("fail") {
                        ("FAIL", Color::Red)
                    } else {
                        ("...", Color::Yellow)
                    };

                    reviewer_lines.push(Line::from(vec![
                        Span::styled(
                            format!("  {symbol} "),
                            Style::default().fg(color).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            format!("{reviewer:<20}"),
                            Style::default().fg(Color::White),
                        ),
                        Span::styled(
                            truncate_str(&verdict, 60),
                            Style::default().fg(Color::DarkGray),
                        ),
                    ]));
                }

                // Also check for active transcript
                if name.starts_with("transcript-") && name.ends_with(".jsonl") {
                    let reviewer = name
                        .strip_prefix("transcript-")
                        .and_then(|s| s.strip_suffix(".jsonl"))
                        .unwrap_or(&name);

                    // If no review-*.md exists yet, show as running
                    let review_file = reviews_dir.join(format!("review-{reviewer}.md"));
                    if !review_file.exists() {
                        reviewer_lines.push(Line::from(vec![
                            Span::styled(
                                "  ... ",
                                Style::default()
                                    .fg(Color::Yellow)
                                    .add_modifier(Modifier::BOLD),
                            ),
                            Span::styled(
                                format!("{reviewer:<20}"),
                                Style::default().fg(Color::White),
                            ),
                            Span::styled(
                                "reviewing...",
                                Style::default().fg(Color::DarkGray),
                            ),
                        ]));
                    }
                }
            }
        }
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Reviewers ");

    if reviewer_lines.is_empty() {
        let p = Paragraph::new("  No reviewer activity yet")
            .style(Style::default().fg(Color::DarkGray))
            .block(block);
        f.render_widget(p, area);
    } else {
        let p = Paragraph::new(reviewer_lines).block(block);
        f.render_widget(p, area);
    }
}

fn draw_help_bar(f: &mut ratatui::Frame, area: Rect) {
    let help = Paragraph::new(Line::from(vec![
        Span::styled(" q", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" quit  "),
        Span::styled("Tab", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" switch run  "),
        Span::styled("j/k", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" scroll  "),
        Span::styled("G", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" bottom  "),
        Span::styled("g", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" top"),
    ]))
    .style(Style::default().fg(Color::DarkGray));

    f.render_widget(help, area);
}

fn has_reviewer_activity(run: &Run) -> bool {
    let reviews_dir = run.dir.join("reviews");
    reviews_dir.is_dir()
        && std::fs::read_dir(&reviews_dir)
            .map(|mut e| e.next().is_some())
            .unwrap_or(false)
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        let end: String = s.chars().take(max - 3).collect();
        format!("{end}...")
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_truncate_str_short() {
        assert_eq!(truncate_str("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_str_exact() {
        assert_eq!(truncate_str("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_str_long() {
        assert_eq!(truncate_str("hello world!", 10), "hello w...");
    }

    #[test]
    fn test_truncate_str_multibyte_utf8() {
        // Should not panic on multi-byte characters
        let s = "héllo wörld café";
        let result = truncate_str(s, 10);
        assert!(result.ends_with("..."));
        assert_eq!(result.chars().count(), 10);
    }

    #[test]
    fn test_style_for_line_bash() {
        let style = style_for_line("[Bash] ls -la");
        assert_eq!(style.fg, Some(Color::Yellow));
    }

    #[test]
    fn test_style_for_line_read() {
        let style = style_for_line("[Read] src/main.rs");
        assert_eq!(style.fg, Some(Color::Cyan));
    }

    #[test]
    fn test_style_for_line_edit() {
        let style = style_for_line("[Edit] src/main.rs");
        assert_eq!(style.fg, Some(Color::Green));
    }

    #[test]
    fn test_style_for_line_write() {
        let style = style_for_line("[Write] src/main.rs");
        assert_eq!(style.fg, Some(Color::Green));
    }

    #[test]
    fn test_style_for_line_grep() {
        let style = style_for_line("[Grep] /pattern/");
        assert_eq!(style.fg, Some(Color::Magenta));
    }

    #[test]
    fn test_style_for_line_glob() {
        let style = style_for_line("[Glob] **/*.rs");
        assert_eq!(style.fg, Some(Color::Magenta));
    }

    #[test]
    fn test_style_for_line_agent() {
        let style = style_for_line("[Agent] explore codebase");
        assert_eq!(style.fg, Some(Color::Blue));
    }

    #[test]
    fn test_style_for_line_unknown_tool() {
        let style = style_for_line("[TodoWrite] update tasks");
        assert_eq!(style.fg, Some(Color::DarkGray));
    }

    #[test]
    fn test_style_for_line_session_started() {
        let style = style_for_line("Session started (model: opus)");
        assert_eq!(style.fg, Some(Color::White));
        assert!(style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn test_style_for_line_session_complete() {
        let style = style_for_line("Session complete (1.0s, $0.05)");
        assert_eq!(style.fg, Some(Color::White));
        assert!(style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn test_style_for_line_thinking() {
        let style = style_for_line("thinking...");
        assert_eq!(style.fg, Some(Color::DarkGray));
    }

    #[test]
    fn test_style_for_line_rate_limit() {
        let style = style_for_line("rate limit check");
        assert_eq!(style.fg, Some(Color::Magenta));
    }

    #[test]
    fn test_style_for_line_plain_text() {
        let style = style_for_line("some output text");
        assert_eq!(style.fg, Some(Color::White));
    }

    #[test]
    fn test_has_reviewer_activity_no_dir() {
        let tmp = TempDir::new().unwrap();
        let run = Run {
            id: "test".to_string(),
            dir: tmp.path().to_path_buf(),
        };
        assert!(!has_reviewer_activity(&run));
    }

    #[test]
    fn test_has_reviewer_activity_empty_dir() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir(tmp.path().join("reviews")).unwrap();
        let run = Run {
            id: "test".to_string(),
            dir: tmp.path().to_path_buf(),
        };
        assert!(!has_reviewer_activity(&run));
    }

    #[test]
    fn test_has_reviewer_activity_with_files() {
        let tmp = TempDir::new().unwrap();
        let reviews = tmp.path().join("reviews");
        std::fs::create_dir(&reviews).unwrap();
        std::fs::write(reviews.join("review-tests.md"), "Verdict: pass").unwrap();
        let run = Run {
            id: "test".to_string(),
            dir: tmp.path().to_path_buf(),
        };
        assert!(has_reviewer_activity(&run));
    }

    #[test]
    fn test_visible_lines_filters_empty() {
        let events = vec![
            Event::Text {
                text: "hello".to_string(),
            },
            Event::ToolResult {
                tool_use_id: "123".to_string(),
            },
            Event::Thinking,
        ];
        let view = RunView {
            run: Run {
                id: "test".to_string(),
                dir: PathBuf::from("/tmp/test"),
            },
            events,
            readers: vec![],
            last_session: 0,
            scroll_offset: 0,
            auto_scroll: true,
        };
        let lines = view.visible_lines();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "hello");
        assert_eq!(lines[1], "thinking...");
    }
}
