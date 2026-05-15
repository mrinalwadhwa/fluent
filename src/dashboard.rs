use anyhow::Result;
use crossterm::event::{self, Event as CEvent, KeyCode, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Tabs};
use ratatui::Terminal;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::run::{self, Run, RunStatus};
use crate::transcript::{self, Event, TranscriptReader};

const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// An agent whose transcript we can display.
struct AgentView {
    name: String,
    events: Vec<Event>,
    /// Pre-rendered lines cache — avoids rebuilding on every frame.
    cached_lines: Vec<String>,
    readers: Vec<TranscriptReader>,
    last_session: u32,
    status: String, // "running", "pass", "fail", "uncertain", ""
}

impl AgentView {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            events: Vec::new(),
            cached_lines: Vec::new(),
            readers: Vec::new(),
            last_session: 0,
            status: String::new(),
        }
    }

    fn poll(&mut self) {
        for reader in &mut self.readers {
            let new_events = reader.read_new();
            for event in &new_events {
                self.cached_lines.extend(event.lines());
            }
            self.events.extend(new_events);
        }
    }
}

/// State for a single run's dashboard view.
struct RunView {
    run: Run,
    /// The directory where sessions/transcripts live — the worktree's run dir
    /// if a worktree exists, otherwise the source run dir.
    live_dir: PathBuf,
    agents: Vec<AgentView>,
    selected_agent: usize,
    scroll_offset: usize,
    auto_scroll: bool,
    /// Wrapped line count from last render, for accurate scroll limits.
    wrapped_total: usize,
}

impl RunView {
    fn new(run: Run) -> Self {
        let live_dir = run.worktree_run_dir().unwrap_or_else(|| run.dir.clone());
        let mut view = Self {
            run,
            live_dir,
            agents: vec![AgentView::new("author")],
            selected_agent: 0,
            scroll_offset: 0,
            auto_scroll: true,
            wrapped_total: 0,
        };
        view.discover_agents();
        view.poll();
        view
    }

    fn discover_agents(&mut self) {
        // Discover author session transcripts
        let author = &mut self.agents[0];
        let transcripts = transcript::list_transcripts(&self.live_dir);
        for (num, path) in transcripts {
            if num > author.last_session {
                author.readers.push(TranscriptReader::new(path));
                author.last_session = num;
            }
        }

        // Discover reviewer transcripts
        let reviews_dir = self.live_dir.join("reviews");
        if reviews_dir.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&reviews_dir) {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if name.starts_with("transcript-") && name.ends_with(".jsonl") {
                        let reviewer = name
                            .strip_prefix("transcript-")
                            .and_then(|s| s.strip_suffix(".jsonl"))
                            .unwrap_or(&name)
                            .to_string();

                        // Add reviewer if not already tracked
                        if !self.agents.iter().any(|a| a.name == reviewer) {
                            let mut agent = AgentView::new(&reviewer);
                            agent.readers.push(TranscriptReader::new(entry.path()));
                            // Check for verdict
                            let review_file =
                                reviews_dir.join(format!("review-{reviewer}.md"));
                            if review_file.exists() {
                                let verdict = std::fs::read_to_string(&review_file)
                                    .ok()
                                    .and_then(|c| {
                                        c.lines()
                                            .find(|l| l.to_lowercase().contains("verdict"))
                                            .map(|l| l.to_string())
                                    })
                                    .unwrap_or_default();
                                if verdict.to_lowercase().contains("pass") {
                                    agent.status = "pass".into();
                                } else if verdict.to_lowercase().contains("fail") {
                                    agent.status = "fail".into();
                                } else if verdict.to_lowercase().contains("uncertain") {
                                    agent.status = "uncertain".into();
                                }
                            } else {
                                agent.status = "running".into();
                            }
                            self.agents.push(agent);
                        } else {
                            // Update status of existing reviewer
                            if let Some(agent) = self.agents.iter_mut().find(|a| a.name == reviewer)
                            {
                                let review_file =
                                    reviews_dir.join(format!("review-{reviewer}.md"));
                                if review_file.exists() && agent.status == "running" {
                                    let verdict = std::fs::read_to_string(&review_file)
                                        .ok()
                                        .and_then(|c| {
                                            c.lines()
                                                .find(|l| l.to_lowercase().contains("verdict"))
                                                .map(|l| l.to_string())
                                        })
                                        .unwrap_or_default();
                                    if verdict.to_lowercase().contains("pass") {
                                        agent.status = "pass".into();
                                    } else if verdict.to_lowercase().contains("fail") {
                                        agent.status = "fail".into();
                                    } else if verdict.to_lowercase().contains("uncertain") {
                                        agent.status = "uncertain".into();
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    fn poll(&mut self) {
        self.discover_agents();
        for agent in &mut self.agents {
            agent.poll();
        }
        if self.auto_scroll {
            self.scroll_to_bottom();
        }
    }

    fn current_agent(&self) -> &AgentView {
        &self.agents[self.selected_agent]
    }

    fn scroll_to_bottom(&mut self) {
        self.scroll_offset = self.wrapped_total;
    }

    fn clamp_scroll(&mut self, visible_height: usize) {
        let max = self.wrapped_total.saturating_sub(visible_height);
        if self.scroll_offset > max {
            self.scroll_offset = max;
        }
    }

    fn visible_lines(&self) -> &[String] {
        &self.current_agent().cached_lines
    }
}

/// Top-level dashboard app state.
struct App {
    runs: Vec<RunView>,
    selected_run: usize,
    search_root: PathBuf,
    should_quit: bool,
    /// Cached activity feed height for scroll clamping.
    feed_height: usize,
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
            feed_height: 20,
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
    crossterm::execute!(stdout, EnterAlternateScreen, crossterm::event::EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_event_loop(&mut terminal, &mut app);

    // Restore terminal
    disable_raw_mode()?;
    crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen, crossterm::event::DisableMouseCapture)?;
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

        // Update feed height from terminal size for scroll clamping
        // Layout: header(3) + runs(3) + agents(3) + margin(1) + feed(rest) + help(1) + borders(2)
        let term_height = terminal.size()?.height as usize;
        app.feed_height = term_height.saturating_sub(3 + 3 + 3 + 1 + 1 + 2);

        // Poll for events with timeout
        let timeout = POLL_INTERVAL
            .checked_sub(last_poll.elapsed())
            .unwrap_or(Duration::ZERO);

        if event::poll(timeout)? {
            let fh = app.feed_height;
            match event::read()? {
            CEvent::Mouse(mouse) => {
                match mouse.kind {
                    crossterm::event::MouseEventKind::ScrollUp => {
                        let view = app.current_view_mut();
                        view.auto_scroll = false;
                        view.clamp_scroll(fh);
                        view.scroll_offset = view.scroll_offset.saturating_sub(3);
                    }
                    crossterm::event::MouseEventKind::ScrollDown => {
                        let view = app.current_view_mut();
                        view.auto_scroll = false;
                        view.clamp_scroll(fh);
                        let max = view.wrapped_total;
                        view.scroll_offset = (view.scroll_offset + 3).min(max);
                    }
                    _ => {}
                }
            }
            CEvent::Key(key) => {
                match (key.modifiers, key.code) {
                    (KeyModifiers::CONTROL, KeyCode::Char('c'))
                    | (_, KeyCode::Char('q')) => {
                        app.should_quit = true;
                    }
                    (_, KeyCode::Tab) => {
                        // Cycle through agents within the current run
                        let view = app.current_view_mut();
                        if !view.agents.is_empty() {
                            view.selected_agent =
                                (view.selected_agent + 1) % view.agents.len();
                            view.scroll_offset = 0;
                            view.auto_scroll = true;
                            view.scroll_to_bottom();
                        }
                    }
                    (_, KeyCode::BackTab) => {
                        let view = app.current_view_mut();
                        if !view.agents.is_empty() {
                            view.selected_agent = if view.selected_agent == 0 {
                                view.agents.len() - 1
                            } else {
                                view.selected_agent - 1
                            };
                            view.scroll_offset = 0;
                            view.auto_scroll = true;
                            view.scroll_to_bottom();
                        }
                    }
                    (_, KeyCode::Right) => {
                        if !app.runs.is_empty() {
                            app.selected_run =
                                (app.selected_run + 1) % app.runs.len();
                        }
                    }
                    (_, KeyCode::Left) => {
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
                        view.clamp_scroll(fh);
                        view.scroll_offset =
                            view.scroll_offset.saturating_sub(1);
                    }
                    (_, KeyCode::Down) | (_, KeyCode::Char('j')) => {
                        let view = app.current_view_mut();
                        view.auto_scroll = false;
                        view.clamp_scroll(fh);
                        let max = view.wrapped_total;
                        view.scroll_offset =
                            (view.scroll_offset + 1).min(max);
                    }
                    (_, KeyCode::Char('G')) | (_, KeyCode::End) => {
                        let view = app.current_view_mut();
                        view.auto_scroll = false;
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
                        view.clamp_scroll(fh);
                        view.scroll_offset =
                            view.scroll_offset.saturating_sub(20);
                    }
                    (_, KeyCode::PageDown) => {
                        let view = app.current_view_mut();
                        view.auto_scroll = false;
                        view.clamp_scroll(fh);
                        let max = view.wrapped_total;
                        view.scroll_offset =
                            (view.scroll_offset + 20).min(max);
                    }
                    _ => {}
                }
            }
            _ => {}
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

fn draw_ui(f: &mut ratatui::Frame, app: &mut App) {
    let size = f.area();
    let idx = app.selected_run;

    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // header
            Constraint::Length(3), // run tabs
            Constraint::Length(3), // agent tabs
            Constraint::Length(1), // margin
            Constraint::Min(10),  // activity feed
            Constraint::Length(1), // help bar
        ])
        .split(size);

    draw_header(f, main_chunks[0], &app.runs[idx]);
    draw_run_tabs(f, main_chunks[1], app);
    draw_agent_tabs(f, main_chunks[2], &app.runs[idx]);
    draw_activity_feed(f, main_chunks[4], &mut app.runs[idx]);
    draw_help_bar(f, main_chunks[5]);
}

fn draw_header(f: &mut ratatui::Frame, area: Rect, view: &RunView) {
    // Read status from worktree (where the agent writes it) with fallback to source
    let status = std::fs::read_to_string(view.live_dir.join("status"))
        .or_else(|_| std::fs::read_to_string(view.run.dir.join("status")))
        .map(|s| s.trim().to_string())
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

    let session_count = view.agents[0].last_session;
    let event_count = view.current_agent().events.len();

    // Determine phase
    let phase = if view.agents.iter().skip(1).any(|a| a.status == "running") {
        let done = view
            .agents
            .iter()
            .skip(1)
            .filter(|a| a.status != "running" && !a.status.is_empty())
            .count();
        let total = view.agents.len() - 1;
        if total > 0 {
            format!("Reviewing ({done}/{total})")
        } else {
            status.clone()
        }
    } else {
        status.clone()
    };

    let header = Paragraph::new(Line::from(vec![
        Span::styled("Run: ", Style::default().fg(Color::DarkGray)),
        Span::styled(&view.run.id, Style::default().add_modifier(Modifier::BOLD)),
        Span::raw("  "),
        Span::styled("Phase: ", Style::default().fg(Color::DarkGray)),
        Span::styled(phase, Style::default().fg(status_color).add_modifier(Modifier::BOLD)),
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

fn draw_agent_tabs(f: &mut ratatui::Frame, area: Rect, view: &RunView) {
    let titles: Vec<Line> = view
        .agents
        .iter()
        .map(|a| {
            let (symbol, color) = match a.status.as_str() {
                "pass" => ("✓", Color::Green),
                "fail" => ("✗", Color::Red),
                "uncertain" => ("?", Color::Yellow),
                "running" => ("⟳", Color::Cyan),
                _ => {
                    if a.name == "author" {
                        ("●", Color::White)
                    } else {
                        ("○", Color::DarkGray)
                    }
                }
            };
            Line::from(Span::styled(
                format!(" {symbol} {} ", a.name),
                Style::default().fg(color),
            ))
        })
        .collect();

    let tabs = Tabs::new(titles)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Agents (Tab to switch) "),
        )
        .select(view.selected_agent)
        .highlight_style(
            Style::default()
                .add_modifier(Modifier::BOLD)
                .add_modifier(Modifier::REVERSED),
        );

    f.render_widget(tabs, area);
}

fn draw_activity_feed(f: &mut ratatui::Frame, area: Rect, view: &mut RunView) {
    let lines = view.visible_lines();
    let content_width = area.width.saturating_sub(2) as usize; // borders
    let visible_height = area.height.saturating_sub(2) as usize;

    // Wrap lines that exceed terminal width, preserving styles
    let mut wrapped: Vec<(String, Style)> = Vec::new();
    for line in lines.iter() {
        let style = style_for_line(line);
        if line.len() <= content_width || content_width == 0 {
            wrapped.push((line.clone(), style));
        } else {
            // Wrap at content_width boundaries
            let mut remaining = line.as_str();
            let mut first = true;
            while !remaining.is_empty() {
                let split_at = remaining
                    .char_indices()
                    .nth(content_width)
                    .map(|(i, _)| i)
                    .unwrap_or(remaining.len());
                let chunk = &remaining[..split_at];
                if first {
                    wrapped.push((chunk.to_string(), style));
                    first = false;
                } else {
                    wrapped.push((format!("  {chunk}"), style));
                }
                remaining = &remaining[split_at..];
            }
        }
    }

    let total = wrapped.len();
    view.wrapped_total = total;
    let start = if view.auto_scroll {
        total.saturating_sub(visible_height)
    } else {
        view.scroll_offset.min(total.saturating_sub(visible_height))
    };
    let end = (start + visible_height).min(total);

    let styled_lines: Vec<Line> = wrapped[start..end]
        .iter()
        .map(|(text, style)| Line::from(Span::styled(text.as_str(), *style)))
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

    let agent_name = &view.current_agent().name;
    let title = format!(
        " {} [{}/{}]{} ",
        agent_name,
        end.min(total),
        total,
        if view.auto_scroll {
            " [auto-scroll]"
        } else {
            ""
        }
    );

    let paragraph = Paragraph::new(styled_lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(title)
            .title_bottom(scroll_indicator),
    );

    f.render_widget(paragraph, area);
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
    } else if line.starts_with("thinking") || line.starts_with("  ") && !line.starts_with("  $") {
        // Thinking blocks and indented tool results in grey
        Style::default().fg(Color::DarkGray)
    } else if line.starts_with("  $") {
        // Command lines within bash tool use
        Style::default().fg(Color::Yellow)
    } else if line.starts_with("rate limit") {
        Style::default().fg(Color::Magenta)
    } else {
        Style::default().fg(Color::White)
    }
}


fn draw_help_bar(f: &mut ratatui::Frame, area: Rect) {
    let help = Paragraph::new(Line::from(vec![
        Span::styled(" q", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" quit  "),
        Span::styled("Tab", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" agent  "),
        Span::styled("←→", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" run  "),
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_visible_lines_filters_empty() {
        let events = vec![
            Event::Text {
                text: "hello".to_string(),
            },
            Event::ToolResult {
                tool_use_id: "123".to_string(),
                content: String::new(),
            },
            Event::Thinking {
                text: "pondering".to_string(),
            },
        ];
        let mut agent = AgentView::new("author");
        for event in &events {
            agent.cached_lines.extend(event.lines());
        }
        agent.events = events;
        let view = RunView {
            run: Run {
                id: "test".to_string(),
                dir: PathBuf::from("/tmp/test"),
            },
            live_dir: PathBuf::from("/tmp/test"),
            agents: vec![agent],
            selected_agent: 0,
            scroll_offset: 0,
            auto_scroll: true,
        };
        let lines = view.visible_lines();
        let non_empty: Vec<&String> = lines.iter().filter(|l| !l.is_empty()).collect();
        assert_eq!(non_empty[0], "hello");
        // Thinking now shows "thinking..." header + indented content
        assert_eq!(non_empty[1], "thinking...");
        assert!(non_empty[2].contains("pondering"));
    }
}
