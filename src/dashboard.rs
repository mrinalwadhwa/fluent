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
use unicode_width::UnicodeWidthStr;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::review;
use crate::run::{self, Run, RunStatus};
use crate::transcript::{self, Event, TranscriptReader};

const RENDER_INTERVAL: Duration = Duration::from_millis(100);
const DATA_POLL_INTERVAL: Duration = Duration::from_millis(2000);
const SPINNER_FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

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
                self.cached_lines
                    .extend(event.lines().into_iter().map(|l| strip_ansi(&l)));
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
    /// Cached run status string, updated on each poll.
    cached_status: String,
}

impl RunView {
    fn new(run: Run) -> Self {
        let live_dir = run.worktree_run_dir().unwrap_or_else(|| run.dir.clone());
        let cached_status = Self::read_status(&live_dir, &run.dir);
        let mut view = Self {
            run,
            live_dir,
            agents: vec![AgentView::new("author")],
            selected_agent: 0,
            scroll_offset: 0,
            auto_scroll: true,
            wrapped_total: 0,
            cached_status,
        };
        view.discover_agents();
        view.poll();
        view
    }

    fn read_status(live_dir: &Path, source_dir: &Path) -> String {
        std::fs::read_to_string(live_dir.join("status"))
            .or_else(|_| std::fs::read_to_string(source_dir.join("status")))
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| "?".into())
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
                                agent.status = verdict_status(&review_file);
                            } else {
                                agent.status = "running".into();
                            }
                            self.agents.push(agent);
                        } else {
                            // Re-evaluate status every poll cycle
                            if let Some(agent) = self.agents.iter_mut().find(|a| a.name == reviewer)
                            {
                                let review_file =
                                    reviews_dir.join(format!("review-{reviewer}.md"));
                                if review_file.exists() {
                                    agent.status = verdict_status(&review_file);
                                } else {
                                    agent.status = "running".into();
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    fn poll(&mut self) {
        // Re-resolve live_dir in case a worktree was created since startup
        let resolved = self.run.worktree_run_dir().unwrap_or_else(|| self.run.dir.clone());
        if resolved != self.live_dir {
            self.live_dir = resolved;
        }
        self.cached_status = Self::read_status(&self.live_dir, &self.run.dir);
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

    /// Scroll up by one line (k/Up handler).
    fn scroll_up(&mut self, visible_height: usize) {
        self.auto_scroll = false;
        self.clamp_scroll(visible_height);
        self.scroll_offset = self.scroll_offset.saturating_sub(1);
    }

    /// Scroll down by one line (j/Down handler). Re-enables auto-scroll at the bottom.
    fn scroll_down(&mut self, visible_height: usize) {
        self.auto_scroll = false;
        self.clamp_scroll(visible_height);
        let max = self.wrapped_total;
        self.scroll_offset = (self.scroll_offset + 1).min(max);
        if self.scroll_offset >= max.saturating_sub(visible_height) {
            self.auto_scroll = true;
        }
    }

    /// Scroll to the end (G/End handler). Re-enables auto-scroll.
    fn scroll_to_end(&mut self) {
        self.auto_scroll = true;
        self.scroll_to_bottom();
    }

    /// Scroll to the top (g/Home handler). Disables auto-scroll.
    fn scroll_to_top(&mut self) {
        self.auto_scroll = false;
        self.scroll_offset = 0;
    }

    /// Scroll up by one page (PageUp handler).
    fn scroll_up_page(&mut self, visible_height: usize) {
        self.auto_scroll = false;
        self.clamp_scroll(visible_height);
        self.scroll_offset = self.scroll_offset.saturating_sub(20);
    }

    /// Scroll down by one page (PageDown handler). Re-enables auto-scroll at the bottom.
    fn scroll_down_page(&mut self, visible_height: usize) {
        self.auto_scroll = false;
        self.clamp_scroll(visible_height);
        let max = self.wrapped_total;
        self.scroll_offset = (self.scroll_offset + 20).min(max);
        if self.scroll_offset >= max.saturating_sub(visible_height) {
            self.auto_scroll = true;
        }
    }

    /// Scroll up by mouse wheel (3 lines). Disables auto-scroll.
    fn mouse_scroll_up(&mut self, visible_height: usize) {
        self.auto_scroll = false;
        self.clamp_scroll(visible_height);
        self.scroll_offset = self.scroll_offset.saturating_sub(3);
    }

    /// Scroll down by mouse wheel (3 lines). Re-enables auto-scroll at the bottom.
    fn mouse_scroll_down(&mut self, visible_height: usize) {
        self.auto_scroll = false;
        self.clamp_scroll(visible_height);
        let max = self.wrapped_total;
        self.scroll_offset = (self.scroll_offset + 3).min(max);
        if self.scroll_offset >= max.saturating_sub(visible_height) {
            self.auto_scroll = true;
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
    /// Monotonically increasing counter, incremented each render frame.
    tick: u64,
    /// First visible run tab index for horizontal scrolling.
    run_tab_offset: usize,
    /// When true, mouse capture is disabled so the user can select text.
    copy_mode: bool,
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
            match views.iter().position(|v| v.run.id == id) {
                Some(pos) => pos,
                None => anyhow::bail!("Run '{}' not found", id),
            }
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

        Ok(Self {
            runs: views,
            selected_run: selected,
            search_root: search_root.to_path_buf(),
            should_quit: false,
            feed_height: 20,
            tick: 0,
            run_tab_offset: 0,
            copy_mode: false,
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

    /// Toggle copy mode and return the new state.
    /// Caller is responsible for issuing the terminal mouse capture command.
    fn toggle_copy_mode(&mut self) -> bool {
        self.copy_mode = !self.copy_mode;
        self.copy_mode
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

    // Restore terminal — only re-disable mouse capture if copy mode hasn't
    // already done so, to avoid sending a redundant escape sequence.
    disable_raw_mode()?;
    if app.copy_mode {
        crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    } else {
        crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen, crossterm::event::DisableMouseCapture)?;
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
        // Render at ~100ms for smooth animation
        if last_render.elapsed() >= RENDER_INTERVAL {
            terminal.draw(|f| draw_ui(f, app))?;
            last_render = Instant::now();
        }

        // Update feed height from terminal size for scroll clamping
        // Layout: header(3) + runs(3) + agents(3) + margin(1) + feed(rest) + help(1) + borders(2)
        let term_height = terminal.size()?.height as usize;
        app.feed_height = term_height.saturating_sub(3 + 3 + 3 + 1 + 1 + 2);

        // Sleep only for the remaining render budget. If we just rendered,
        // this is nearly RENDER_INTERVAL; if we skipped, it may be zero.
        let timeout = RENDER_INTERVAL
            .checked_sub(last_render.elapsed())
            .unwrap_or(Duration::ZERO);

        if event::poll(timeout)? {
            let fh = app.feed_height;
            match event::read()? {
            CEvent::Mouse(mouse) if !app.runs.is_empty() => {
                match mouse.kind {
                    crossterm::event::MouseEventKind::ScrollUp => {
                        app.current_view_mut().mouse_scroll_up(fh);
                    }
                    crossterm::event::MouseEventKind::ScrollDown => {
                        app.current_view_mut().mouse_scroll_down(fh);
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
                    (_, KeyCode::Tab) if !app.runs.is_empty() => {
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
                    (_, KeyCode::BackTab) if !app.runs.is_empty() => {
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
                    (_, KeyCode::Right) if !app.runs.is_empty() => {
                        app.selected_run =
                            (app.selected_run + 1) % app.runs.len();
                    }
                    (_, KeyCode::Left) if !app.runs.is_empty() => {
                        app.selected_run = if app.selected_run == 0 {
                            app.runs.len() - 1
                        } else {
                            app.selected_run - 1
                        };
                    }
                    (_, KeyCode::Up) | (_, KeyCode::Char('k'))
                        if !app.runs.is_empty() =>
                    {
                        app.current_view_mut().scroll_up(fh);
                    }
                    (_, KeyCode::Down) | (_, KeyCode::Char('j'))
                        if !app.runs.is_empty() =>
                    {
                        app.current_view_mut().scroll_down(fh);
                    }
                    (_, KeyCode::Char('G')) | (_, KeyCode::End)
                        if !app.runs.is_empty() =>
                    {
                        app.current_view_mut().scroll_to_end();
                    }
                    (_, KeyCode::Char('g')) | (_, KeyCode::Home)
                        if !app.runs.is_empty() =>
                    {
                        app.current_view_mut().scroll_to_top();
                    }
                    (_, KeyCode::PageUp) if !app.runs.is_empty() => {
                        app.current_view_mut().scroll_up_page(fh);
                    }
                    (_, KeyCode::PageDown) if !app.runs.is_empty() => {
                        app.current_view_mut().scroll_down_page(fh);
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

        // Periodic poll for new data at a slower cadence
        if last_data_poll.elapsed() >= DATA_POLL_INTERVAL {
            app.poll();
            last_data_poll = Instant::now();
        }
    }

    Ok(())
}

fn verdict_status(review_file: &Path) -> String {
    let content = std::fs::read_to_string(review_file).unwrap_or_default();
    match review::extract_verdict(&content) {
        review::Verdict::Pass => "pass".into(),
        review::Verdict::Fail => "fail".into(),
        review::Verdict::Uncertain => "uncertain".into(),
    }
}

fn draw_ui(f: &mut ratatui::Frame, app: &mut App) {
    let size = f.area();
    let tick = app.tick;
    app.tick += 1;

    if app.runs.is_empty() {
        let main_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // header
                Constraint::Min(3),    // empty state
                Constraint::Length(1), // help bar
            ])
            .split(size);

        let header = Paragraph::new(Line::from(vec![
            Span::styled("No runs found", Style::default().fg(Color::DarkGray)),
        ]))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Factory Dashboard "),
        );
        f.render_widget(header, main_chunks[0]);

        let empty = Paragraph::new(Line::from(Span::styled(
            "No runs in this project. Create a brief to start a run.",
            Style::default().fg(Color::DarkGray),
        )))
        .block(Block::default().borders(Borders::ALL).title(" Runs "));
        f.render_widget(empty, main_chunks[1]);
        draw_help_bar(f, main_chunks[2], app.copy_mode);
        return;
    }

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

    draw_header(f, main_chunks[0], &app.runs[idx], tick);
    draw_run_tabs(f, main_chunks[1], app);
    draw_agent_tabs(f, main_chunks[2], &app.runs[idx]);
    draw_activity_feed(f, main_chunks[4], &mut app.runs[idx]);
    draw_help_bar(f, main_chunks[5], app.copy_mode);
}

/// Compute the phase display for the header.
///
/// Returns (phase_text, color, is_animated).
fn compute_phase(view: &RunView, status: &str) -> (String, Color, bool) {
    let reviewers_running = view.agents.iter().skip(1).any(|a| a.status == "running");
    let has_reviewers = view.agents.len() > 1;

    if reviewers_running {
        let done = view
            .agents
            .iter()
            .skip(1)
            .filter(|a| a.status != "running" && !a.status.is_empty())
            .count();
        let total = view.agents.len() - 1;
        (format!("Reviewing {done}/{total}"), Color::Cyan, true)
    } else if has_reviewers
        && view.agents.iter().skip(1).all(|a| !a.status.is_empty() && a.status != "running")
    {
        // All reviewers done, check overall status
        match status {
            "complete" | "landed" => ("Complete".into(), Color::Blue, false),
            "failed" => ("Failed".into(), Color::Red, false),
            "needs-user" => ("Needs input".into(), Color::Yellow, false),
            _ => (status.into(), Color::White, false),
        }
    } else {
        match status {
            "executing" => ("Executing".into(), Color::Green, true),
            "reviewing" => ("Reviewing".into(), Color::Cyan, true),
            "complete" => ("Complete".into(), Color::Blue, false),
            "failed" => ("Failed".into(), Color::Red, false),
            "needs-user" => ("Needs input".into(), Color::Yellow, false),
            "rate-limited" => ("Rate limited".into(), Color::Magenta, true),
            "planned" => ("Planned".into(), Color::Cyan, false),
            _ => (status.into(), Color::White, false),
        }
    }
}

fn draw_header(f: &mut ratatui::Frame, area: Rect, view: &RunView, tick: u64) {
    let (phase_text, phase_color, animated) = compute_phase(view, &view.cached_status);

    let spinner = if animated {
        let frame = SPINNER_FRAMES[(tick % SPINNER_FRAMES.len() as u64) as usize];
        format!(" {frame}")
    } else {
        String::new()
    };

    let session_count = view.agents[0].last_session;
    let event_count = view.current_agent().events.len();

    let header = Paragraph::new(Line::from(vec![
        Span::styled("Run: ", Style::default().fg(Color::DarkGray)),
        Span::styled(&view.run.id, Style::default().add_modifier(Modifier::BOLD)),
        Span::raw("  "),
        Span::styled(
            format!("{phase_text}{spinner}"),
            Style::default().fg(phase_color).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled("Session: ", Style::default().fg(Color::DarkGray)),
        Span::styled(format!("{session_count}"), Style::default()),
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

/// Build the display label for a single run tab.
fn run_tab_label(v: &RunView) -> (String, Color) {
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
    (format!(" {} [{}] ", v.run.id, status), color)
}

/// Compute which tabs are visible starting from `offset` within `content_width`.
///
/// Returns `(visible_end, selected_visible)` where `visible_end` is the
/// exclusive upper bound of the visible range, and `selected_visible`
/// indicates whether `selected` falls within that range.
fn visible_tab_range(
    labels: &[String],
    offset: usize,
    selected: usize,
    content_width: usize,
) -> (usize, bool) {
    let mut used = if offset > 0 { 2 } else { 0 }; // "◀ "
    let mut visible_end = offset;
    let mut selected_visible = false;

    for i in offset..labels.len() {
        let label_w = labels[i].width();
        let separator = if i > offset { 3 } else { 0 }; // " │ "
        let needed = used + separator + label_w;
        let right_arrow = if i + 1 < labels.len() { 2 } else { 0 };
        if needed + right_arrow > content_width && i != offset {
            break;
        }
        used = needed;
        visible_end = i + 1;
        if i == selected {
            selected_visible = true;
        }
    }

    (visible_end, selected_visible)
}

/// Ensure `run_tab_offset` keeps the selected run visible within `width`.
fn clamp_run_tab_offset(app: &mut App, content_width: usize) {
    if app.runs.is_empty() {
        app.run_tab_offset = 0;
        return;
    }

    let labels: Vec<String> = app.runs.iter().map(|v| run_tab_label(v).0).collect();

    // Ensure selected is at least at offset
    if app.selected_run < app.run_tab_offset {
        app.run_tab_offset = app.selected_run;
    }

    // Walk forward from offset until selected is visible
    loop {
        let (_, selected_visible) =
            visible_tab_range(&labels, app.run_tab_offset, app.selected_run, content_width);

        if selected_visible || app.run_tab_offset >= app.selected_run {
            break;
        }
        app.run_tab_offset += 1;
    }
}

fn draw_run_tabs(f: &mut ratatui::Frame, area: Rect, app: &mut App) {
    let content_width = area.width.saturating_sub(2) as usize; // borders

    clamp_run_tab_offset(app, content_width);

    let has_left = app.run_tab_offset > 0;
    let labels: Vec<(String, Color)> = app.runs.iter().map(|v| run_tab_label(v)).collect();
    let label_strings: Vec<String> = labels.iter().map(|(s, _)| s.clone()).collect();

    let (visible_end, _) = visible_tab_range(
        &label_strings,
        app.run_tab_offset,
        app.selected_run,
        content_width,
    );
    let has_right = visible_end < labels.len();

    // Build the spans
    let mut spans: Vec<Span> = Vec::new();
    if has_left {
        spans.push(Span::styled("◀ ", Style::default().fg(Color::DarkGray)));
    }
    for i in app.run_tab_offset..visible_end {
        if i > app.run_tab_offset {
            spans.push(Span::styled(" │ ", Style::default().fg(Color::DarkGray)));
        }
        let (ref label, color) = labels[i];
        let style = if i == app.selected_run {
            Style::default()
                .fg(color)
                .add_modifier(Modifier::BOLD)
                .add_modifier(Modifier::REVERSED)
        } else {
            Style::default().fg(color)
        };
        spans.push(Span::styled(label.as_str(), style));
    }
    if has_right {
        spans.push(Span::styled(" ▶", Style::default().fg(Color::DarkGray)));
    }

    let line = Line::from(spans);
    let paragraph = Paragraph::new(line)
        .block(Block::default().borders(Borders::ALL).title(" Runs "));

    f.render_widget(paragraph, area);
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
    let indent = "  ";
    let indent_width = 2;
    for line in lines.iter() {
        let style = style_for_line(line);
        if line.width() <= content_width || content_width == 0 {
            wrapped.push((line.clone(), style));
        } else {
            // Wrap at content_width display-column boundaries
            let mut remaining = line.as_str();
            let mut first = true;
            while !remaining.is_empty() {
                let max_cols = if first {
                    content_width
                } else {
                    content_width.saturating_sub(indent_width)
                };
                let split_at = split_at_width(remaining, max_cols);
                let chunk = &remaining[..split_at];
                if first {
                    wrapped.push((chunk.to_string(), style));
                    first = false;
                } else {
                    wrapped.push((format!("{indent}{chunk}"), style));
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

/// Strip ANSI escape sequences (CSI and OSC) from a string.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            match chars.peek() {
                Some('[') => {
                    // CSI sequence: consume until final byte (0x40-0x7E)
                    chars.next();
                    while let Some(&c) = chars.peek() {
                        chars.next();
                        if c as u32 >= 0x40 && c as u32 <= 0x7E {
                            break;
                        }
                    }
                }
                Some(']') => {
                    // OSC sequence: consume until ST (ESC\ or BEL)
                    chars.next();
                    while let Some(&c) = chars.peek() {
                        if c == '\x07' {
                            chars.next();
                            break;
                        }
                        if c == '\x1b' {
                            chars.next();
                            if chars.peek() == Some(&'\\') {
                                chars.next();
                            }
                            break;
                        }
                        chars.next();
                    }
                }
                _ => {
                    // Unknown escape — skip just the ESC
                }
            }
        } else if ch.is_control() && ch != '\t' && ch != '\n' {
            // Skip other control characters
        } else {
            out.push(ch);
        }
    }
    out
}

/// Find the byte index at which `s` reaches `max_cols` display columns.
fn split_at_width(s: &str, max_cols: usize) -> usize {
    let mut cols = 0;
    for (i, ch) in s.char_indices() {
        let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if cols + w > max_cols {
            return i;
        }
        cols += w;
    }
    s.len()
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


fn draw_help_bar(f: &mut ratatui::Frame, area: Rect, copy_mode: bool) {
    let mut spans = vec![
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
        Span::raw(" top  "),
        Span::styled("c", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" copy"),
    ];
    if copy_mode {
        spans.push(Span::styled(
            "  [COPY MODE]",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }

    let help = Paragraph::new(Line::from(spans))
        .style(Style::default().fg(Color::DarkGray));

    f.render_widget(help, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;

    // --- Helpers for rendering tests ---

    /// Create a synthetic RunView without filesystem access.
    fn make_run_view(id: &str, agents: Vec<AgentView>) -> RunView {
        make_run_view_with_status(id, agents, "executing")
    }

    fn make_run_view_with_status(id: &str, agents: Vec<AgentView>, status: &str) -> RunView {
        RunView {
            run: Run {
                id: id.to_string(),
                dir: PathBuf::from("/tmp/test"),
            },
            live_dir: PathBuf::from("/tmp/test"),
            agents,
            selected_agent: 0,
            scroll_offset: 0,
            auto_scroll: true,
            wrapped_total: 0,
            cached_status: status.to_string(),
        }
    }

    /// Render draw_header to a TestBackend and return the buffer text.
    fn render_header(view: &RunView, tick: u64) -> String {
        let backend = TestBackend::new(80, 3);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw_header(f, f.area(), view, tick);
            })
            .unwrap();
        buffer_text(terminal.backend().buffer())
    }

    /// Render agent tabs to a TestBackend and return the buffer text.
    fn render_agent_tabs(view: &RunView) -> String {
        let backend = TestBackend::new(80, 3);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw_agent_tabs(f, f.area(), view);
            })
            .unwrap();
        buffer_text(terminal.backend().buffer())
    }

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

    /// Check if any cell in the buffer contains one of the spinner frame characters.
    fn has_spinner(buf: &Buffer) -> bool {
        let area = buf.area;
        for y in area.y..area.y + area.height {
            for x in area.x..area.x + area.width {
                let cell = &buf[(x, y)];
                let ch = cell.symbol().chars().next().unwrap_or(' ');
                if SPINNER_FRAMES.contains(&ch) {
                    return true;
                }
            }
        }
        false
    }

    /// Render header to TestBackend and return the buffer directly.
    fn render_header_buf(view: &RunView, tick: u64) -> Buffer {
        let backend = TestBackend::new(80, 3);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw_header(f, f.area(), view, tick);
            })
            .unwrap();
        terminal.backend().buffer().clone()
    }

    // --- Style tests ---

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
        let view = make_run_view("test", vec![agent]);
        let lines = view.visible_lines();
        let non_empty: Vec<&String> = lines.iter().filter(|l| !l.is_empty()).collect();
        assert_eq!(non_empty[0], "hello");
        assert_eq!(non_empty[1], "thinking...");
        assert!(non_empty[2].contains("pondering"));
    }

    // --- Phase detection tests ---

    #[test]
    fn test_compute_phase_executing() {
        let view = make_run_view("run-1", vec![AgentView::new("author")]);
        let (text, color, animated) = compute_phase(&view, "executing");
        assert_eq!(text, "Executing");
        assert_eq!(color, Color::Green);
        assert!(animated);
    }

    #[test]
    fn test_compute_phase_complete() {
        let view = make_run_view("run-1", vec![AgentView::new("author")]);
        let (text, color, animated) = compute_phase(&view, "complete");
        assert_eq!(text, "Complete");
        assert_eq!(color, Color::Blue);
        assert!(!animated);
    }

    #[test]
    fn test_compute_phase_failed() {
        let view = make_run_view("run-1", vec![AgentView::new("author")]);
        let (text, color, animated) = compute_phase(&view, "failed");
        assert_eq!(text, "Failed");
        assert_eq!(color, Color::Red);
        assert!(!animated);
    }

    #[test]
    fn test_compute_phase_needs_user() {
        let view = make_run_view("run-1", vec![AgentView::new("author")]);
        let (text, color, animated) = compute_phase(&view, "needs-user");
        assert_eq!(text, "Needs input");
        assert_eq!(color, Color::Yellow);
        assert!(!animated);
    }

    #[test]
    fn test_compute_phase_rate_limited() {
        let view = make_run_view("run-1", vec![AgentView::new("author")]);
        let (text, color, animated) = compute_phase(&view, "rate-limited");
        assert_eq!(text, "Rate limited");
        assert_eq!(color, Color::Magenta);
        assert!(animated);
    }

    #[test]
    fn test_compute_phase_planned() {
        let view = make_run_view("run-1", vec![AgentView::new("author")]);
        let (text, color, animated) = compute_phase(&view, "planned");
        assert_eq!(text, "Planned");
        assert_eq!(color, Color::Cyan);
        assert!(!animated);
    }

    #[test]
    fn test_compute_phase_reviewing_active() {
        let mut r1 = AgentView::new("tests");
        r1.status = "running".into();
        let mut r2 = AgentView::new("arch");
        r2.status = "pass".into();
        let view = make_run_view("run-1", vec![AgentView::new("author"), r1, r2]);
        let (text, color, animated) = compute_phase(&view, "executing");
        assert_eq!(text, "Reviewing 1/2");
        assert_eq!(color, Color::Cyan);
        assert!(animated);
    }

    #[test]
    fn test_compute_phase_reviewing_all_running() {
        let mut r1 = AgentView::new("tests");
        r1.status = "running".into();
        let mut r2 = AgentView::new("arch");
        r2.status = "running".into();
        let view = make_run_view("run-1", vec![AgentView::new("author"), r1, r2]);
        let (text, color, animated) = compute_phase(&view, "executing");
        assert_eq!(text, "Reviewing 0/2");
        assert_eq!(color, Color::Cyan);
        assert!(animated);
    }

    #[test]
    fn test_compute_phase_reviewing_no_reviewers() {
        let view = make_run_view("run-1", vec![AgentView::new("author")]);
        let (text, color, animated) = compute_phase(&view, "reviewing");
        assert_eq!(text, "Reviewing");
        assert_eq!(color, Color::Cyan);
        assert!(animated);
    }

    #[test]
    fn test_compute_phase_all_reviewers_done() {
        let mut r1 = AgentView::new("tests");
        r1.status = "pass".into();
        let mut r2 = AgentView::new("arch");
        r2.status = "fail".into();
        let view = make_run_view("run-1", vec![AgentView::new("author"), r1, r2]);
        let (text, color, animated) = compute_phase(&view, "complete");
        assert_eq!(text, "Complete");
        assert_eq!(color, Color::Blue);
        assert!(!animated);
    }

    // --- Rendering tests using TestBackend ---

    #[test]
    fn test_header_author_executing_shows_spinner() {
        let view = make_run_view("test-run", vec![AgentView::new("author")]);
        let buf = render_header_buf(&view, 0);
        let text = buffer_text(&buf);
        assert!(text.contains("Executing"));
        assert!(has_spinner(&buf));
    }

    #[test]
    fn test_header_spinner_advances_with_tick() {
        let view = make_run_view("test-run", vec![AgentView::new("author")]);
        let text_t0 = render_header(&view, 0);
        let text_t1 = render_header(&view, 1);
        // Different ticks produce different spinner characters
        assert_ne!(text_t0, text_t1);
        // Both should contain "Executing"
        assert!(text_t0.contains("Executing"));
        assert!(text_t1.contains("Executing"));
    }

    #[test]
    fn test_header_reviewing_shows_progress() {
        let mut r1 = AgentView::new("tests");
        r1.status = "running".into();
        let mut r2 = AgentView::new("arch");
        r2.status = "pass".into();
        let mut r3 = AgentView::new("docs");
        r3.status = "running".into();
        let view = make_run_view(
            "test-run",
            vec![AgentView::new("author"), r1, r2, r3],
        );
        let buf = render_header_buf(&view, 5);
        let text = buffer_text(&buf);
        assert!(text.contains("Reviewing"));
        assert!(text.contains("1/3"));
        assert!(has_spinner(&buf));
    }

    #[test]
    fn test_header_rate_limited_shows_spinner() {
        let view =
            make_run_view_with_status("test-run", vec![AgentView::new("author")], "rate-limited");
        let buf = render_header_buf(&view, 0);
        let text = buffer_text(&buf);
        assert!(text.contains("Rate limited"));
        assert!(has_spinner(&buf));
    }

    #[test]
    fn test_header_complete_no_spinner() {
        let mut r1 = AgentView::new("tests");
        r1.status = "pass".into();
        let view = make_run_view_with_status(
            "test-run",
            vec![AgentView::new("author"), r1],
            "complete",
        );
        let buf = render_header_buf(&view, 0);
        let text = buffer_text(&buf);
        assert!(text.contains("Complete"));
        assert!(!has_spinner(&buf));
    }

    #[test]
    fn test_header_failed_no_spinner() {
        let view = make_run_view_with_status("test-run", vec![AgentView::new("author")], "failed");
        let buf = render_header_buf(&view, 0);
        let text = buffer_text(&buf);
        assert!(text.contains("Failed"));
        assert!(!has_spinner(&buf));
    }

    #[test]
    fn test_agent_tab_shows_verdict_immediately() {
        let mut r1 = AgentView::new("tests");
        r1.status = "pass".into();
        let mut r2 = AgentView::new("arch");
        r2.status = "fail".into();
        let view = make_run_view(
            "test-run",
            vec![AgentView::new("author"), r1, r2],
        );
        let text = render_agent_tabs(&view);
        assert!(text.contains("✓"));
        assert!(text.contains("✗"));
    }

    #[test]
    fn test_agent_tab_running_shows_spinner_symbol() {
        let mut r1 = AgentView::new("tests");
        r1.status = "running".into();
        let view = make_run_view("test-run", vec![AgentView::new("author"), r1]);
        let text = render_agent_tabs(&view);
        assert!(text.contains("⟳"));
    }

    #[test]
    fn test_stale_state_refresh() {
        // Verify that the agent tab renders the running symbol before update
        // and the pass symbol after — confirming the render path reflects
        // status changes without delay.
        let mut r1 = AgentView::new("tests");
        r1.status = "running".into();
        let view = make_run_view("test-run", vec![AgentView::new("author"), r1]);

        // First render: shows running indicator
        let text1 = render_agent_tabs(&view);
        assert!(text1.contains("⟳"));

        // Second render with updated status: shows verdict immediately
        let mut r1_updated = AgentView::new("tests");
        r1_updated.status = "pass".into();
        let view2 = make_run_view("test-run", vec![AgentView::new("author"), r1_updated]);

        let text2 = render_agent_tabs(&view2);
        assert!(text2.contains("✓"));
        assert!(!text2.contains("⟳"));
    }

    #[test]
    fn test_discover_agents_updates_verdict() {
        // Verify that discover_agents() re-evaluates reviewer status on each
        // poll cycle. A reviewer starts as "running" (transcript present, no
        // review file), then after the review file appears, discover_agents()
        // updates the status to the verdict.
        use std::fs;
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let run_dir = tmp.path().to_path_buf();
        let reviews_dir = run_dir.join("reviews");
        fs::create_dir_all(&reviews_dir).unwrap();

        // Write a transcript file so the reviewer is discovered
        fs::write(
            reviews_dir.join("transcript-behaviors.jsonl"),
            "{}",
        )
        .unwrap();

        let mut view = RunView {
            run: Run {
                id: "test-run".to_string(),
                dir: run_dir.clone(),
            },
            live_dir: run_dir.clone(),
            agents: vec![AgentView::new("author")],
            selected_agent: 0,
            scroll_offset: 0,
            auto_scroll: true,
            wrapped_total: 0,
            cached_status: "executing".to_string(),
        };

        // First discover: no review file yet → status should be "running"
        view.discover_agents();
        assert_eq!(view.agents.len(), 2);
        assert_eq!(view.agents[1].name, "behaviors");
        assert_eq!(view.agents[1].status, "running");

        // Write the review file with a pass verdict
        fs::write(
            reviews_dir.join("review-behaviors.md"),
            "Verdict: pass\n\nAll good.",
        )
        .unwrap();

        // Second discover: review file exists → status updates to "pass"
        view.discover_agents();
        assert_eq!(view.agents[1].status, "pass");
    }

    #[test]
    fn test_compute_phase_all_reviewers_done_failed() {
        // When all reviewers are done and the run status is "failed",
        // compute_phase should return "Failed" via the has_reviewers branch.
        let mut r1 = AgentView::new("tests");
        r1.status = "pass".into();
        let mut r2 = AgentView::new("arch");
        r2.status = "fail".into();
        let view = make_run_view("run-1", vec![AgentView::new("author"), r1, r2]);
        let (text, color, animated) = compute_phase(&view, "failed");
        assert_eq!(text, "Failed");
        assert_eq!(color, Color::Red);
        assert!(!animated);
    }

    // --- Wrapping tests ---

    #[test]
    fn test_split_at_width_ascii() {
        assert_eq!(split_at_width("hello world", 5), 5);
        assert_eq!(split_at_width("hello", 10), 5);
        assert_eq!(split_at_width("", 5), 0);
    }

    #[test]
    fn test_split_at_width_multibyte() {
        // Each CJK char is 2 columns wide
        let s = "\u{4e16}\u{754c}hello"; // "世界hello"
        // 世=2cols, 界=2cols, h=1col => first 4 cols = "世界"
        assert_eq!(split_at_width(s, 4), 6); // 2 chars * 3 bytes each
        // 5 cols: can't fit 世界h (2+2+1=5), can fit
        assert_eq!(split_at_width(s, 5), 7); // 世界h = 6+1 bytes
    }

    #[test]
    fn test_split_at_width_wide_char_boundary() {
        // If max_cols lands in the middle of a wide char, don't include it
        let s = "\u{4e16}\u{754c}"; // "世界" — 4 columns
        assert_eq!(split_at_width(s, 3), 3); // Only "世" fits (2 cols), 界 needs 2 more
    }

    #[test]
    fn test_activity_feed_wrapping_no_cutoff() {
        // A line exactly at content_width should not be wrapped
        let mut agent = AgentView::new("author");
        // 40 chars for a 40-col content area
        agent.cached_lines = vec!["a".repeat(40)];

        let mut view = make_run_view("test-run", vec![agent]);
        // Render in a 42-wide area (40 content + 2 border)
        let backend = TestBackend::new(42, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                let area = Rect::new(0, 0, 42, 5);
                draw_activity_feed(f, area, &mut view);
            })
            .unwrap();
        let text = buffer_text(terminal.backend().buffer());
        // Full line should appear without truncation
        assert!(text.contains(&"a".repeat(40)));
    }

    #[test]
    fn test_activity_feed_wrapping_continuation_not_truncated() {
        // A long line should wrap and continuation should not lose chars
        let mut agent = AgentView::new("author");
        // 50 chars in a 20-col content area → wraps
        let long_line = "abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMN";
        agent.cached_lines = vec![long_line.to_string()];

        let mut view = make_run_view("test-run", vec![agent]);
        // 22-wide area (20 content + 2 border), tall enough to show wrapped lines
        let backend = TestBackend::new(22, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                let area = Rect::new(0, 0, 22, 8);
                draw_activity_feed(f, area, &mut view);
            })
            .unwrap();
        let text = buffer_text(terminal.backend().buffer());
        // All characters from the original line should appear somewhere in output
        for ch in long_line.chars() {
            assert!(
                text.contains(ch),
                "Character '{ch}' missing from wrapped output"
            );
        }
    }

    // --- strip_ansi tests (guards against stray "A" rendering bug) ---

    #[test]
    fn test_strip_ansi_csi_sequence() {
        // CSI color codes should be fully removed
        let input = "\x1b[31mhello\x1b[0m";
        assert_eq!(strip_ansi(input), "hello");
    }

    #[test]
    fn test_strip_ansi_osc_sequence() {
        // OSC sequences (e.g. terminal title) should be removed
        let input = "\x1b]0;title\x07text";
        assert_eq!(strip_ansi(input), "text");
    }

    #[test]
    fn test_strip_ansi_csi_terminator_preserves_next_char() {
        // CSI sequence "\x1b[?25h" ends at 'h'; the 'A' after it
        // must not be consumed. This was the original "stray A" bug.
        let input = "\x1b[?25hA visible text";
        let result = strip_ansi(input);
        assert_eq!(result, "A visible text");
    }

    #[test]
    fn test_strip_ansi_bare_esc_consumes_only_esc() {
        // Bare ESC not followed by [ or ] — only ESC itself is consumed
        let input = "\x1bXhello";
        let result = strip_ansi(input);
        assert_eq!(result, "Xhello");
    }

    #[test]
    fn test_strip_ansi_preserves_plain_text() {
        assert_eq!(strip_ansi("hello world"), "hello world");
        assert_eq!(strip_ansi(""), "");
    }

    #[test]
    fn test_strip_ansi_removes_control_chars() {
        // Control characters other than \t and \n should be removed
        let input = "a\x01b\x02c";
        assert_eq!(strip_ansi(input), "abc");
    }

    // --- Scrollable run tab tests ---

    fn make_app_with_runs(ids: &[&str], selected: usize) -> App {
        let views: Vec<RunView> = ids
            .iter()
            .map(|id| make_run_view(id, vec![AgentView::new("author")]))
            .collect();
        App {
            runs: views,
            selected_run: selected,
            search_root: PathBuf::from("/tmp"),
            should_quit: false,
            feed_height: 20,
            tick: 0,
            run_tab_offset: 0,
            copy_mode: false,
        }
    }

    fn render_run_tabs_text(app: &mut App, width: u16) -> String {
        let backend = TestBackend::new(width, 3);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw_run_tabs(f, f.area(), app);
            })
            .unwrap();
        buffer_text(terminal.backend().buffer())
    }

    #[test]
    fn test_run_tabs_all_fit_no_arrows() {
        let mut app = make_app_with_runs(&["run-a", "run-b"], 0);
        let text = render_run_tabs_text(&mut app, 80);
        assert!(text.contains("run-a"));
        assert!(text.contains("run-b"));
        assert!(!text.contains('◀'));
        assert!(!text.contains('▶'));
    }

    #[test]
    fn test_run_tabs_overflow_shows_right_arrow() {
        // Use many long run IDs in a narrow terminal
        let mut app = make_app_with_runs(
            &[
                "20260101-first-run",
                "20260102-second-run",
                "20260103-third-run",
                "20260104-fourth-run",
            ],
            0,
        );
        let text = render_run_tabs_text(&mut app, 50);
        // First run should be visible since it's selected
        assert!(text.contains("20260101"));
        // Should show right arrow indicating more runs
        assert!(text.contains('▶'));
    }

    #[test]
    fn test_run_tabs_selected_always_visible() {
        let mut app = make_app_with_runs(
            &[
                "20260101-first-run",
                "20260102-second-run",
                "20260103-third-run",
                "20260104-fourth-run",
                "20260105-fifth-run",
            ],
            4, // select the last run
        );
        let text = render_run_tabs_text(&mut app, 50);
        // Last run must be visible
        assert!(text.contains("20260105"));
        // Should show left arrow since earlier runs are hidden
        assert!(text.contains('◀'));
    }

    #[test]
    fn test_run_tabs_far_right_selection_visible() {
        let mut app = make_app_with_runs(
            &[
                "20260101-first",
                "20260102-second",
                "20260103-third",
                "20260104-fourth",
            ],
            0,
        );
        // Set selection to the last run (no key-handling, just state)
        app.selected_run = 3;
        let text = render_run_tabs_text(&mut app, 40);
        assert!(text.contains("20260104"));
    }

    #[test]
    fn test_run_tabs_backward_scroll_shows_selected() {
        let mut app = make_app_with_runs(
            &[
                "20260101-first",
                "20260102-second",
                "20260103-third",
                "20260104-fourth",
            ],
            0,
        );
        // Artificially set offset past the selected run
        app.run_tab_offset = 2;
        app.selected_run = 0;
        let text = render_run_tabs_text(&mut app, 50);
        // Selected run must be visible after backward clamp
        assert!(text.contains("20260101"));
    }

    #[test]
    fn test_run_tabs_empty_no_panic() {
        let mut app = App {
            runs: Vec::new(),
            selected_run: 0,
            search_root: PathBuf::from("/tmp"),
            should_quit: false,
            feed_height: 20,
            tick: 0,
            run_tab_offset: 0,
            copy_mode: false,
        };

        // Should render the empty state without panicking
        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw_ui(f, &mut app);
            })
            .unwrap();
        let text = buffer_text(terminal.backend().buffer());
        assert!(text.contains("No runs"));
    }

    #[test]
    fn test_run_tabs_single_run_no_arrows() {
        let mut app = make_app_with_runs(&["only-run"], 0);
        let text = render_run_tabs_text(&mut app, 80);
        assert!(text.contains("only-run"));
        assert!(!text.contains('◀'));
        assert!(!text.contains('▶'));
    }

    #[test]
    fn test_clamp_run_tab_offset_keeps_selected_visible() {
        let mut app = make_app_with_runs(
            &["run-0", "run-1", "run-2", "run-3", "run-4"],
            4,
        );
        app.run_tab_offset = 0;
        // With a narrow width, offset must advance to show run-4
        clamp_run_tab_offset(&mut app, 30);
        assert!(app.run_tab_offset > 0);
    }

    // --- ANSI + multibyte rendering test (stray character guard) ---

    #[test]
    fn test_activity_feed_ansi_multibyte_no_stray_chars() {
        // Lines with ANSI escapes should render cleanly after stripping.
        // The CSI sequence "\x1b[?25h" must not swallow the following 'A'.
        let mut agent = AgentView::new("author");
        agent.cached_lines = vec![
            strip_ansi("\x1b[31m[Bash]\x1b[0m ls -la"),
            strip_ansi("\x1b[?25hA visible text after cursor show"),
            strip_ansi("\x1b[32m\x1b[1mcolored bold\x1b[0m normal"),
        ];

        let mut view = make_run_view("test-run", vec![agent]);
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                let area = Rect::new(0, 0, 60, 10);
                draw_activity_feed(f, area, &mut view);
            })
            .unwrap();
        let text = buffer_text(terminal.backend().buffer());

        // ANSI sequences stripped, content preserved
        assert!(text.contains("[Bash] ls -la"));
        assert!(text.contains("A visible text"));
        assert!(text.contains("colored bold"));
    }

    #[test]
    fn test_activity_feed_multibyte_wrapping() {
        // CJK characters (2-column wide) should wrap without losing chars.
        let mut agent = AgentView::new("author");
        // 10 CJK chars = 20 display columns, plus ASCII = wraps in 22-col area
        let line = "\u{4e16}\u{754c}\u{4f60}\u{597d}\u{4e16}\u{754c}\u{4f60}\u{597d}\u{4e16}\u{754c} hello";
        agent.cached_lines = vec![line.to_string()];

        let mut view = make_run_view("test-run", vec![agent]);
        let backend = TestBackend::new(22, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                let area = Rect::new(0, 0, 22, 8);
                draw_activity_feed(f, area, &mut view);
            })
            .unwrap();
        let text = buffer_text(terminal.backend().buffer());

        // All characters should appear somewhere in the wrapped output
        assert!(text.contains("hello"));
        // At least some CJK chars rendered
        assert!(text.contains('\u{4e16}') || text.contains('\u{754c}'));
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
        let mut app = make_app_with_runs(&["run-1"], 0);
        assert!(!app.copy_mode);

        // First toggle enables copy mode
        assert!(app.toggle_copy_mode());
        assert!(app.copy_mode);

        // Second toggle disables copy mode
        assert!(!app.toggle_copy_mode());
        assert!(!app.copy_mode);
    }

    // --- Auto-scroll re-enable tests ---

    #[test]
    fn test_scroll_to_bottom_enables_auto_scroll() {
        // G/End should re-enable auto-scroll
        let mut view = make_run_view("test-run", vec![AgentView::new("author")]);
        view.auto_scroll = false;
        view.scroll_offset = 10;
        view.wrapped_total = 100;

        view.scroll_to_end();

        assert!(view.auto_scroll);
        assert_eq!(view.scroll_offset, 100);
    }

    #[test]
    fn test_scroll_down_reenables_auto_scroll_at_bottom() {
        // j/Down should re-enable auto-scroll when reaching the bottom
        let mut view = make_run_view("test-run", vec![AgentView::new("author")]);
        view.wrapped_total = 30;
        view.auto_scroll = false;
        let fh = 20;
        view.scroll_offset = view.wrapped_total.saturating_sub(fh) - 1;

        view.scroll_down(fh);

        assert!(view.auto_scroll);
    }

    #[test]
    fn test_page_down_reenables_auto_scroll_at_bottom() {
        // PageDown should re-enable auto-scroll when reaching the bottom
        let mut view = make_run_view("test-run", vec![AgentView::new("author")]);
        view.wrapped_total = 30;
        view.auto_scroll = false;
        let fh = 20;
        // Position near the bottom so a 20-line page reaches it
        view.scroll_offset = view.wrapped_total.saturating_sub(fh) - 5;

        view.scroll_down_page(fh);

        assert!(view.auto_scroll);
    }

    #[test]
    fn test_mouse_scroll_down_reenables_auto_scroll_at_bottom() {
        // Mouse scroll down should re-enable auto-scroll when reaching the bottom
        let mut view = make_run_view("test-run", vec![AgentView::new("author")]);
        view.wrapped_total = 30;
        view.auto_scroll = false;
        let fh = 20;
        // Position so that scrolling 3 lines reaches the bottom
        view.scroll_offset = view.wrapped_total.saturating_sub(fh) - 1;

        view.mouse_scroll_down(fh);

        assert!(view.auto_scroll);
    }
}
