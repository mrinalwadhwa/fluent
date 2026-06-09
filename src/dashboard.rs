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
use ratatui::widgets::{Block, Borders, Paragraph, Tabs};
use std::collections::BTreeSet;
use std::fs::Metadata;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthStr;

use crate::cleanup;
use crate::review;
use crate::run::{self, Run};
use crate::transcript::{self, Event, TranscriptReader};
use crate::work_status::{self, WorkStatus};

const RENDER_INTERVAL: Duration = Duration::from_millis(75);
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
    transcript_source: Option<TranscriptSource>,
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
            transcript_source: None,
        }
    }

    fn new_static(name: &str, content: &str) -> Self {
        let mut view = Self::new(name);
        view.cached_lines = content.lines().map(strip_ansi).collect();
        view
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct TranscriptSource {
    file_id: FileId,
    len: u64,
}

impl TranscriptSource {
    fn from_path(path: &Path) -> Option<Self> {
        let metadata = std::fs::metadata(path).ok()?;
        Some(Self {
            file_id: FileId::from_metadata(&metadata),
            len: metadata.len(),
        })
    }

    fn still_current_for(&self, path: &Path) -> bool {
        let Some(current) = Self::from_path(path) else {
            return false;
        };

        current.file_id == self.file_id && current.len >= self.len
    }
}

#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct FileId {
    dev: u64,
    ino: u64,
}

#[cfg(unix)]
impl FileId {
    fn from_metadata(metadata: &Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;

        Self {
            dev: metadata.dev(),
            ino: metadata.ino(),
        }
    }
}

#[cfg(not(unix))]
#[derive(Debug, Clone, PartialEq, Eq)]
struct FileId {
    modified: Option<std::time::SystemTime>,
}

#[cfg(not(unix))]
impl FileId {
    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            modified: metadata.modified().ok(),
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
    agent_selection_touched: bool,
    scroll_offset: usize,
    auto_scroll: bool,
    /// Wrapped line count from last render, for accurate scroll limits.
    wrapped_total: usize,
    /// Cached run status string, updated on each poll.
    cached_status: String,
}

impl RunView {
    fn new(run: Run) -> Self {
        let live_dir = run.live_artifact_dir();
        let cached_status = run
            .effective_status()
            .map(|status| status.to_string())
            .unwrap_or_else(|_| "?".into());
        let mut view = Self {
            run,
            live_dir,
            agents: vec![AgentView::new("author")],
            selected_agent: 0,
            agent_selection_touched: false,
            scroll_offset: 0,
            auto_scroll: true,
            wrapped_total: 0,
            cached_status,
        };
        view.discover_agents();
        view.sync_report_view();
        view.poll();
        view
    }

    fn report_path(&self) -> Option<PathBuf> {
        let live_report = self.live_dir.join("report.md");
        if live_report.exists() {
            return Some(live_report);
        }

        let source_report = self.run.dir.join("report.md");
        if source_report.exists() {
            Some(source_report)
        } else {
            None
        }
    }

    fn should_default_to_report(&self) -> bool {
        matches!(self.cached_status.as_str(), "complete" | "landed")
            && !self.agent_selection_touched
    }

    fn sync_report_view(&mut self) {
        let Some(path) = self.report_path() else {
            return;
        };
        let Ok(content) = std::fs::read_to_string(path) else {
            return;
        };

        let report_idx = if let Some(idx) = self.agents.iter().position(|a| a.name == "report") {
            self.agents[idx] = AgentView::new_static("report", &content);
            idx
        } else {
            let idx = self.agents.len().min(1);
            self.agents
                .insert(idx, AgentView::new_static("report", &content));
            if self.selected_agent >= idx {
                self.selected_agent += 1;
            }
            idx
        };

        if self.should_default_to_report() {
            self.selected_agent = report_idx;
            self.scroll_offset = 0;
            self.auto_scroll = true;
        }
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

        // Discover current-round reviewer artifacts.
        let reviews_dir = self.live_dir.join("reviews");
        if reviews_dir.is_dir() {
            let mut current_reviewers = BTreeSet::new();
            if let Ok(entries) = std::fs::read_dir(&reviews_dir) {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if let Some(reviewer) = reviewer_from_transcript_name(&name)
                        .or_else(|| reviewer_from_review_name(&name))
                    {
                        current_reviewers.insert(reviewer);
                    }
                }
            }
            for reviewer in &current_reviewers {
                self.sync_reviewer_agent(&reviews_dir, reviewer);
            }
            self.retain_current_reviewers(&current_reviewers);
        } else {
            self.retain_current_reviewers(&BTreeSet::new());
        }
    }

    fn sync_reviewer_agent(&mut self, reviews_dir: &Path, reviewer: &str) {
        let transcript_path = reviews_dir.join(format!("transcript-{reviewer}.jsonl"));
        let review_path = reviews_dir.join(format!("review-{reviewer}.md"));

        if transcript_path.exists() {
            let transcript_source = TranscriptSource::from_path(&transcript_path);
            if let Some(agent) = self.agents.iter_mut().find(|a| a.name == reviewer) {
                if !agent_transcript_matches(agent, &transcript_path) {
                    *agent = reviewer_agent(reviewer, &transcript_path, transcript_source);
                } else {
                    agent.transcript_source = transcript_source;
                }
                update_reviewer_status(agent, reviews_dir, reviewer);
            } else {
                let mut agent = reviewer_agent(reviewer, &transcript_path, transcript_source);
                update_reviewer_status(&mut agent, reviews_dir, reviewer);
                self.agents.push(agent);
            }
            return;
        }

        if review_path.exists() {
            let mut agent = reviewer_review_agent(reviewer, &review_path);
            update_reviewer_status(&mut agent, reviews_dir, reviewer);
            if let Some(existing) = self.agents.iter_mut().find(|a| a.name == reviewer) {
                *existing = agent;
            } else {
                self.agents.push(agent);
            }
        }
    }

    fn retain_current_reviewers(&mut self, current_reviewers: &BTreeSet<String>) {
        let selected_name = self
            .agents
            .get(self.selected_agent)
            .map(|agent| agent.name.clone());

        self.agents.retain(|agent| {
            matches!(agent.name.as_str(), "author" | "report")
                || current_reviewers.contains(&agent.name)
        });

        if let Some(selected_name) = selected_name {
            if let Some(index) = self
                .agents
                .iter()
                .position(|agent| agent.name == selected_name)
            {
                self.selected_agent = index;
                return;
            }
        }

        self.selected_agent = 0;
        self.scroll_offset = 0;
        self.auto_scroll = true;
    }

    fn poll(&mut self) {
        // Re-resolve live_dir in case a worktree was created since startup
        let resolved = self.run.live_artifact_dir();
        if resolved != self.live_dir {
            self.live_dir = resolved;
        }
        self.cached_status = self
            .run
            .effective_status()
            .map(|status| status.to_string())
            .unwrap_or_else(|_| "?".into());
        self.discover_agents();
        self.sync_report_view();
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

    fn review_state_summary(&self) -> Option<String> {
        match review::effective_review_state(&self.live_dir, &self.run.dir) {
            review::ReviewStateRead::Present(state) => Some(format!(
                "{} ({})",
                state.state.as_str(),
                state.source.as_str()
            )),
            review::ReviewStateRead::Invalid(_) => Some("invalid review-state.json".to_string()),
            review::ReviewStateRead::Missing => None,
        }
    }

    fn select_next_agent(&mut self) {
        if self.agents.is_empty() {
            return;
        }

        self.agent_selection_touched = true;
        self.selected_agent = (self.selected_agent + 1) % self.agents.len();
        self.scroll_offset = 0;
        self.auto_scroll = true;
        self.scroll_to_bottom();
    }

    fn select_previous_agent(&mut self) {
        if self.agents.is_empty() {
            return;
        }

        self.agent_selection_touched = true;
        self.selected_agent = if self.selected_agent == 0 {
            self.agents.len() - 1
        } else {
            self.selected_agent - 1
        };
        self.scroll_offset = 0;
        self.auto_scroll = true;
        self.scroll_to_bottom();
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

fn reviewer_agent(
    reviewer: &str,
    transcript_path: &Path,
    transcript_source: Option<TranscriptSource>,
) -> AgentView {
    let mut agent = AgentView::new(reviewer);
    agent
        .readers
        .push(TranscriptReader::new(transcript_path.to_path_buf()));
    agent.transcript_source = transcript_source;
    agent
}

fn reviewer_review_agent(reviewer: &str, review_path: &Path) -> AgentView {
    let content = std::fs::read_to_string(review_path).unwrap_or_default();
    AgentView::new_static(reviewer, &content)
}

fn reviewer_from_transcript_name(name: &str) -> Option<String> {
    name.strip_prefix("transcript-")
        .and_then(|s| s.strip_suffix(".jsonl"))
        .map(str::to_string)
}

fn reviewer_from_review_name(name: &str) -> Option<String> {
    name.strip_prefix("review-")
        .and_then(|s| s.strip_suffix(".md"))
        .map(str::to_string)
}

fn agent_transcript_matches(agent: &AgentView, transcript_path: &Path) -> bool {
    let Some(source) = &agent.transcript_source else {
        return false;
    };

    agent
        .readers
        .iter()
        .any(|reader| reader.path == transcript_path && source.still_current_for(transcript_path))
}

fn update_reviewer_status(agent: &mut AgentView, reviews_dir: &Path, reviewer: &str) {
    let review_file = reviews_dir.join(format!("review-{reviewer}.md"));
    if review_file.exists() {
        agent.status = verdict_status(&review_file);
    } else {
        agent.status = "running".into();
    }
}

/// Top-level dashboard app state.
struct App {
    runs: Vec<RunView>,
    work_status: WorkStatus,
    show_work: bool,
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

        let mut views: Vec<RunView> = all_runs.into_iter().map(|r| RunView::new(r)).collect();
        if target_run_id.is_none() {
            sort_views_for_dashboard(&mut views);
        }
        let work_status = work_status::load_work_status(search_root)?;
        let show_work = target_run_id.is_none() && views.is_empty() && !work_status.is_empty();

        // Find the index of the target run, or pick the first active one
        let selected = if let Some(id) = target_run_id {
            match views.iter().position(|v| v.run.id == id) {
                Some(pos) => pos,
                None => anyhow::bail!("Run '{}' not found", id),
            }
        } else {
            views
                .iter()
                .position(|v| matches!(v.cached_status.as_str(), "executing" | "planned"))
                .unwrap_or(0)
        };

        Ok(Self {
            runs: views,
            show_work,
            work_status,
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
        let selected_id = self.runs.get(self.selected_run).map(|v| v.run.id.clone());
        let selected_index = self.selected_run;

        if let Ok(all_runs) = run::list_runs(&self.search_root) {
            let current_ids: Vec<String> = all_runs.iter().map(|r| r.id.clone()).collect();

            self.runs.retain(|v| current_ids.contains(&v.run.id));

            for r in all_runs {
                if !self.runs.iter().any(|v| v.run.id == r.id) {
                    self.runs.push(RunView::new(r));
                }
            }
        }

        for view in &mut self.runs {
            view.poll();
        }

        sort_views_for_dashboard(&mut self.runs);
        self.selected_run = selected_id
            .and_then(|id| self.runs.iter().position(|v| v.run.id == id))
            .unwrap_or_else(|| {
                if self.runs.is_empty() {
                    0
                } else {
                    selected_index.min(self.runs.len() - 1)
                }
            });
        if let Ok(work_status) = work_status::load_work_status(&self.search_root) {
            self.work_status = work_status;
        }
        if self.runs.is_empty() && !self.work_status.is_empty() {
            self.show_work = true;
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

fn sort_views_for_dashboard(views: &mut [RunView]) {
    views.sort_by(|a, b| {
        dashboard_view_priority(a)
            .cmp(&dashboard_view_priority(b))
            .then_with(|| a.run.id.cmp(&b.run.id))
    });
}

fn dashboard_view_priority(view: &RunView) -> u8 {
    if cleanup::run_is_cleaned(&view.run) {
        return 2;
    }

    let actionable = matches!(
        view.cached_status.as_str(),
        "planned" | "executing" | "reviewing" | "needs-user" | "failed"
    );
    if actionable { 0 } else { 1 }
}

/// Launch the dashboard TUI.
pub fn run_dashboard(search_root: &Path, run_id: Option<&str>) -> Result<()> {
    let mut app = App::new(search_root, run_id)?;

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
        // Render frequently enough for spinner animation to feel responsive.
        if last_render.elapsed() >= RENDER_INTERVAL {
            terminal.draw(|f| draw_ui(f, app))?;
            last_render = Instant::now();
        }

        // Update feed height from terminal size for scroll clamping
        // Layout: header(3) + view tabs(3) + runs(3) + agents(3) + margin(1)
        // + feed(rest) + help(1) + borders(2)
        let term_height = terminal.size()?.height as usize;
        app.feed_height = term_height.saturating_sub(3 + 3 + 3 + 3 + 1 + 1 + 2);

        // Sleep only for the remaining render budget. If we just rendered,
        // this is nearly RENDER_INTERVAL; if we skipped, it may be zero.
        let timeout = RENDER_INTERVAL
            .checked_sub(last_render.elapsed())
            .unwrap_or(Duration::ZERO);

        if event::poll(timeout)? {
            let fh = app.feed_height;
            match event::read()? {
                CEvent::Mouse(mouse) if !app.runs.is_empty() && !app.show_work => {
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
                    (_, KeyCode::Char('w')) => {
                        app.show_work = true;
                    }
                    (_, KeyCode::Char('r')) if !app.runs.is_empty() => {
                        app.show_work = false;
                    }
                    (_, KeyCode::Tab) if !app.runs.is_empty() && !app.show_work => {
                        app.current_view_mut().select_next_agent();
                    }
                    (_, KeyCode::BackTab) if !app.runs.is_empty() && !app.show_work => {
                        app.current_view_mut().select_previous_agent();
                    }
                    (_, KeyCode::Right) if !app.runs.is_empty() && !app.show_work => {
                        app.selected_run = (app.selected_run + 1) % app.runs.len();
                    }
                    (_, KeyCode::Left) if !app.runs.is_empty() && !app.show_work => {
                        app.selected_run = if app.selected_run == 0 {
                            app.runs.len() - 1
                        } else {
                            app.selected_run - 1
                        };
                    }
                    (_, KeyCode::Up) | (_, KeyCode::Char('k'))
                        if !app.runs.is_empty() && !app.show_work =>
                    {
                        app.current_view_mut().scroll_up(fh);
                    }
                    (_, KeyCode::Down) | (_, KeyCode::Char('j'))
                        if !app.runs.is_empty() && !app.show_work =>
                    {
                        app.current_view_mut().scroll_down(fh);
                    }
                    (_, KeyCode::Char('G')) | (_, KeyCode::End)
                        if !app.runs.is_empty() && !app.show_work =>
                    {
                        app.current_view_mut().scroll_to_end();
                    }
                    (_, KeyCode::Char('g')) | (_, KeyCode::Home)
                        if !app.runs.is_empty() && !app.show_work =>
                    {
                        app.current_view_mut().scroll_to_top();
                    }
                    (_, KeyCode::PageUp) if !app.runs.is_empty() && !app.show_work => {
                        app.current_view_mut().scroll_up_page(fh);
                    }
                    (_, KeyCode::PageDown) if !app.runs.is_empty() && !app.show_work => {
                        app.current_view_mut().scroll_down_page(fh);
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

    if app.show_work {
        let main_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // header
                Constraint::Length(3), // view tabs
                Constraint::Min(6),    // work items
                Constraint::Length(1), // help bar
            ])
            .split(size);

        draw_work_header(f, main_chunks[0], app);
        draw_view_tabs(f, main_chunks[1], app);
        draw_work_items(f, main_chunks[2], &app.work_status);
        draw_help_bar(f, main_chunks[3], app.copy_mode, app.show_work);
        return;
    }

    if app.runs.is_empty() && app.work_status.is_empty() {
        let main_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // header
                Constraint::Length(3), // view tabs
                Constraint::Min(3),    // empty state
                Constraint::Length(1), // help bar
            ])
            .split(size);

        let header = Paragraph::new(Line::from(vec![Span::styled(
            "No runs found",
            Style::default().fg(Color::DarkGray),
        )]))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Factory Dashboard "),
        );
        f.render_widget(header, main_chunks[0]);
        draw_view_tabs(f, main_chunks[1], app);

        let empty = Paragraph::new(Line::from(Span::styled(
            "No runs in this project. Create a brief to start a run.",
            Style::default().fg(Color::DarkGray),
        )))
        .block(Block::default().borders(Borders::ALL).title(" Runs "));
        f.render_widget(empty, main_chunks[2]);
        draw_help_bar(f, main_chunks[3], app.copy_mode, app.show_work);
        return;
    }

    let idx = app.selected_run;

    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // header
            Constraint::Length(3), // view tabs
            Constraint::Length(3), // run tabs
            Constraint::Length(3), // agent tabs
            Constraint::Length(1), // margin
            Constraint::Min(10),   // activity feed
            Constraint::Length(1), // help bar
        ])
        .split(size);

    let any_activity = app.runs.iter().any(run_view_has_activity);
    draw_header(f, main_chunks[0], &app.runs[idx], tick, any_activity);
    draw_view_tabs(f, main_chunks[1], app);
    draw_run_tabs(f, main_chunks[2], app);
    draw_agent_tabs(f, main_chunks[3], &app.runs[idx]);
    draw_activity_feed(f, main_chunks[5], &mut app.runs[idx]);
    draw_help_bar(f, main_chunks[6], app.copy_mode, app.show_work);
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
            .title(" Factory Dashboard "),
    );

    f.render_widget(header, area);
}

fn draw_view_tabs(f: &mut ratatui::Frame, area: Rect, app: &App) {
    let runs_label = format!(" Runs ({}) ", app.runs.len());
    let work_label = format!(" Work Items ({}) ", app.work_status.rows.len());
    let titles = vec![
        Line::from(Span::styled(runs_label, Style::default().fg(Color::Blue))),
        Line::from(Span::styled(work_label, Style::default().fg(Color::Cyan))),
    ];
    let tabs = Tabs::new(titles)
        .block(Block::default().borders(Borders::ALL).title(" Views "))
        .select(usize::from(app.show_work))
        .highlight_style(
            Style::default()
                .add_modifier(Modifier::BOLD)
                .add_modifier(Modifier::REVERSED),
        );
    f.render_widget(tabs, area);
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

/// Compute the phase display for the header.
///
/// Returns (phase_text, color, is_animated).
fn compute_phase(view: &RunView, status: &str) -> (String, Color, bool) {
    let reviewer_statuses: Vec<&str> = view
        .agents
        .iter()
        .filter(|a| a.name != "author" && a.name != "report")
        .map(|a| a.status.as_str())
        .collect();
    let reviewers_running = reviewer_statuses.iter().any(|status| *status == "running");
    let has_reviewers = !reviewer_statuses.is_empty();

    if reviewers_running {
        let done = reviewer_statuses
            .iter()
            .filter(|status| **status != "running" && !status.is_empty())
            .count();
        let total = reviewer_statuses.len();
        (format!("Reviewing {done}/{total}"), Color::Cyan, true)
    } else if has_reviewers
        && reviewer_statuses
            .iter()
            .all(|status| !status.is_empty() && *status != "running")
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

fn draw_header(f: &mut ratatui::Frame, area: Rect, view: &RunView, tick: u64, any_activity: bool) {
    let (phase_text, phase_color, animated) = compute_phase(view, &view.cached_status);

    let spinner = if animated {
        let frame = SPINNER_FRAMES[(tick % SPINNER_FRAMES.len() as u64) as usize];
        format!(" {frame}")
    } else {
        String::new()
    };

    let session_count = view.agents[0].last_session;
    let event_count = view.current_agent().events.len();
    let review_state = view.review_state_summary();

    let mut spans = vec![
        Span::styled("Run: ", Style::default().fg(Color::DarkGray)),
        Span::styled(&view.run.id, Style::default().add_modifier(Modifier::BOLD)),
        Span::raw("  "),
        Span::styled(
            format!("{phase_text}{spinner}"),
            Style::default()
                .fg(phase_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled("Session: ", Style::default().fg(Color::DarkGray)),
        Span::styled(format!("{session_count}"), Style::default()),
        Span::raw("  "),
        Span::styled("Events: ", Style::default().fg(Color::DarkGray)),
        Span::styled(format!("{event_count}"), Style::default()),
    ];
    if let Some(review_state) = review_state {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            "Review: ",
            Style::default().fg(Color::DarkGray),
        ));
        spans.push(Span::styled(review_state, Style::default().fg(Color::Blue)));
    }

    let header = Paragraph::new(Line::from(spans)).block(
        Block::default()
            .borders(Borders::ALL)
            .title(dashboard_title(tick, any_activity)),
    );

    f.render_widget(header, area);
}

fn dashboard_title(tick: u64, any_activity: bool) -> String {
    if any_activity {
        let frame = SPINNER_FRAMES[(tick % SPINNER_FRAMES.len() as u64) as usize];
        format!(" Factory Dashboard {frame} ")
    } else {
        " Factory Dashboard ".into()
    }
}

fn run_view_has_activity(view: &RunView) -> bool {
    matches!(
        view.cached_status.as_str(),
        "executing" | "reviewing" | "rate-limited"
    ) || view.agents.iter().any(|agent| agent.status == "running")
}

/// Build the display label for a single run tab.
fn run_tab_label(v: &RunView, tick: u64) -> (String, Color) {
    let status = &v.cached_status;
    let color = match status.as_str() {
        "executing" => Color::Green,
        "reviewing" | "planned" => Color::Cyan,
        "rate-limited" => Color::Magenta,
        "complete" => Color::Blue,
        "failed" => Color::Red,
        "needs-user" => Color::Yellow,
        _ => Color::White,
    };
    let marker = run_tab_status_marker(status, tick);
    (format!(" {} [{marker}{status}] ", v.run.id), color)
}

fn run_tab_status_marker(status: &str, tick: u64) -> String {
    match status {
        "executing" | "reviewing" | "rate-limited" => {
            let frame = SPINNER_FRAMES[(tick % SPINNER_FRAMES.len() as u64) as usize];
            format!("{frame} ")
        }
        "planned" => "… ".into(),
        _ => String::new(),
    }
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

    let labels: Vec<String> = app
        .runs
        .iter()
        .map(|v| run_tab_label(v, app.tick).0)
        .collect();

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
    let labels: Vec<(String, Color)> = app
        .runs
        .iter()
        .map(|v| run_tab_label(v, app.tick))
        .collect();
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
    let paragraph =
        Paragraph::new(line).block(Block::default().borders(Borders::ALL).title(" Runs "));

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
                    } else if a.name == "report" {
                        ("■", Color::Blue)
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

fn draw_help_bar(f: &mut ratatui::Frame, area: Rect, copy_mode: bool, show_work: bool) {
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
        Span::styled("r", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" runs  "),
        Span::styled("w", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" work  "),
    ]);
    if !show_work {
        spans.extend([
            Span::styled("Tab", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" agent  "),
            Span::styled("←→", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" run  "),
        ]);
    }
    spans.extend([
        Span::styled("j/k", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" scroll  "),
        Span::styled("G", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" bottom  "),
        Span::styled("g", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" top  "),
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
    use tempfile::TempDir;

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
            agent_selection_touched: false,
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
                draw_header(f, f.area(), view, tick, false);
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

    #[test]
    fn test_run_view_review_state_summary_prefers_state_file() {
        let tmp = TempDir::new().unwrap();
        let run_dir = tmp.path().join(".factory/runs/test-run");
        std::fs::create_dir_all(run_dir.join("reviews")).unwrap();
        std::fs::write(run_dir.join("status"), "complete").unwrap();
        std::fs::write(run_dir.join("reviews/review-tests.md"), "Verdict: fail").unwrap();
        std::fs::write(
            run_dir.join("review-state.json"),
            r#"{
  "state": "accepted-review-limit",
  "round": 11,
  "source": "review-limit",
  "verdicts": {
    "tests": "fail"
  },
  "max_rounds": 10,
  "reason": "Review round limit reached with a clean worktree."
}
"#,
        )
        .unwrap();

        let view = RunView {
            run: Run {
                id: "test-run".to_string(),
                dir: run_dir.clone(),
            },
            live_dir: run_dir,
            agents: vec![AgentView::new("author")],
            selected_agent: 0,
            agent_selection_touched: false,
            scroll_offset: 0,
            auto_scroll: true,
            wrapped_total: 0,
            cached_status: "complete".to_string(),
        };

        assert_eq!(
            view.review_state_summary(),
            Some("accepted-review-limit (review-limit)".to_string())
        );
    }

    #[test]
    fn test_run_view_review_state_summary_reports_invalid_state_file() {
        let tmp = TempDir::new().unwrap();
        let run_dir = tmp.path().join(".factory/runs/test-run");
        std::fs::create_dir_all(run_dir.join("reviews")).unwrap();
        std::fs::write(run_dir.join("status"), "complete").unwrap();
        std::fs::write(run_dir.join("reviews/review-tests.md"), "Verdict: pass").unwrap();
        std::fs::write(run_dir.join("review-state.json"), r#"{"state":"unknown"}"#).unwrap();

        let view = RunView {
            run: Run {
                id: "test-run".to_string(),
                dir: run_dir.clone(),
            },
            live_dir: run_dir,
            agents: vec![AgentView::new("author")],
            selected_agent: 0,
            agent_selection_touched: false,
            scroll_offset: 0,
            auto_scroll: true,
            wrapped_total: 0,
            cached_status: "complete".to_string(),
        };

        assert_eq!(
            view.review_state_summary(),
            Some("invalid review-state.json".to_string())
        );
    }

    fn render_activity_feed_text(view: &mut RunView) -> String {
        let backend = TestBackend::new(100, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw_activity_feed(f, f.area(), view);
            })
            .unwrap();
        buffer_text(terminal.backend().buffer())
    }

    fn write_author_transcript(run_dir: &Path, text: &str) {
        let session_dir = run_dir.join("sessions/session-1");
        std::fs::create_dir_all(&session_dir).unwrap();
        std::fs::write(
            session_dir.join("transcript.jsonl"),
            format!(
                r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"{text}"}}]}}}}"#
            ),
        )
        .unwrap();
    }

    fn write_reviewer_transcript(run_dir: &Path, reviewer: &str, text: &str) {
        let reviews_dir = run_dir.join("reviews");
        std::fs::create_dir_all(&reviews_dir).unwrap();
        std::fs::write(
            reviews_dir.join(format!("transcript-{reviewer}.jsonl")),
            format!(
                r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"{text}"}}]}}}}"#
            ),
        )
        .unwrap();
        std::fs::write(
            reviews_dir.join(format!("review-{reviewer}.md")),
            "Verdict: pass\n",
        )
        .unwrap();
    }

    fn make_filesystem_run(run_dir: &Path, id: &str, status: &str) -> Run {
        std::fs::create_dir_all(run_dir).unwrap();
        std::fs::write(run_dir.join("status"), status).unwrap();
        std::fs::write(run_dir.join("brief.md"), format!("Brief for {id}")).unwrap();
        Run {
            id: id.to_string(),
            dir: run_dir.to_path_buf(),
        }
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
                draw_header(f, f.area(), view, tick, false);
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

    #[test]
    fn test_completed_run_with_report_shows_report_by_default() {
        let tmp = tempfile::TempDir::new().unwrap();
        let run_dir = tmp.path().join("done-run");
        let run = make_filesystem_run(&run_dir, "done-run", "complete");
        write_author_transcript(&run_dir, "final author transcript");
        std::fs::write(
            run_dir.join("report.md"),
            "# Run Report\n\nCross-session summary",
        )
        .unwrap();

        let mut view = RunView::new(run);

        assert_eq!(view.current_agent().name, "report");
        assert!(render_agent_tabs(&view).contains("report"));
        let text = render_activity_feed_text(&mut view);
        assert!(text.contains("Cross-session summary"));
        assert!(!text.contains("final author transcript"));
    }

    #[test]
    fn test_completed_run_without_report_shows_author_transcript() {
        let tmp = tempfile::TempDir::new().unwrap();
        let run_dir = tmp.path().join("done-run");
        let run = make_filesystem_run(&run_dir, "done-run", "complete");
        write_author_transcript(&run_dir, "final author transcript");

        let mut view = RunView::new(run);

        assert_eq!(view.current_agent().name, "author");
        let text = render_activity_feed_text(&mut view);
        assert!(text.contains("final author transcript"));
        assert!(!render_agent_tabs(&view).contains("report"));
    }

    #[test]
    fn test_nonterminal_run_with_report_shows_author_transcript() {
        let tmp = tempfile::TempDir::new().unwrap();
        let run_dir = tmp.path().join("active-run");
        let run = make_filesystem_run(&run_dir, "active-run", "executing");
        write_author_transcript(&run_dir, "live author transcript");
        std::fs::write(run_dir.join("report.md"), "Stale report content").unwrap();

        let mut view = RunView::new(run);

        assert_eq!(view.current_agent().name, "author");
        assert!(render_agent_tabs(&view).contains("report"));
        let text = render_activity_feed_text(&mut view);
        assert!(text.contains("live author transcript"));
        assert!(!text.contains("Stale report content"));
    }

    #[test]
    fn test_active_run_defaults_to_report_after_completion_poll() {
        let tmp = tempfile::TempDir::new().unwrap();
        let run_dir = tmp.path().join("active-run");
        let run = make_filesystem_run(&run_dir, "active-run", "executing");
        write_author_transcript(&run_dir, "live author transcript");

        let mut view = RunView::new(run);
        assert_eq!(view.current_agent().name, "author");

        std::fs::write(run_dir.join("report.md"), "Final report summary").unwrap();
        std::fs::write(run_dir.join("status"), "complete").unwrap();
        view.poll();

        assert_eq!(view.current_agent().name, "report");
        let text = render_activity_feed_text(&mut view);
        assert!(text.contains("Final report summary"));
        assert!(!text.contains("live author transcript"));
    }

    #[test]
    fn test_completion_poll_keeps_touched_transcript_selection() {
        let tmp = tempfile::TempDir::new().unwrap();
        let run_dir = tmp.path().join("active-run");
        let run = make_filesystem_run(&run_dir, "active-run", "executing");
        write_author_transcript(&run_dir, "author transcript content");
        write_reviewer_transcript(&run_dir, "behaviors", "reviewer transcript content");

        let mut view = RunView::new(run);
        view.select_next_agent();
        assert_eq!(view.current_agent().name, "behaviors");

        std::fs::write(run_dir.join("report.md"), "Final report summary").unwrap();
        std::fs::write(run_dir.join("status"), "landed").unwrap();
        view.poll();

        assert_eq!(view.current_agent().name, "behaviors");
        assert!(render_agent_tabs(&view).contains("report"));
        let text = render_activity_feed_text(&mut view);
        assert!(text.contains("reviewer transcript content"));
        assert!(!text.contains("Final report summary"));
    }

    #[test]
    fn test_report_view_keeps_transcript_tabs_accessible() {
        let tmp = tempfile::TempDir::new().unwrap();
        let run_dir = tmp.path().join("done-run");
        let run = make_filesystem_run(&run_dir, "done-run", "complete");
        write_author_transcript(&run_dir, "author transcript content");
        write_reviewer_transcript(&run_dir, "behaviors", "reviewer transcript content");
        std::fs::write(run_dir.join("report.md"), "Report summary content").unwrap();

        let mut view = RunView::new(run);
        assert_eq!(view.current_agent().name, "report");

        view.select_previous_agent();
        assert_eq!(view.current_agent().name, "author");
        let author_text = render_activity_feed_text(&mut view);
        assert!(author_text.contains("author transcript content"));

        view.select_next_agent();
        view.select_next_agent();
        assert_eq!(view.current_agent().name, "behaviors");
        let reviewer_text = render_activity_feed_text(&mut view);
        assert!(reviewer_text.contains("reviewer transcript content"));
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
    fn test_header_reviewing_status_shows_spinner_without_reviewers() {
        let view =
            make_run_view_with_status("test-run", vec![AgentView::new("author")], "reviewing");
        let buf = render_header_buf(&view, 0);
        let text = buffer_text(&buf);
        assert!(text.contains("Reviewing"));
        assert!(has_spinner(&buf));
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
    fn test_dashboard_title_shows_global_activity() {
        assert_eq!(dashboard_title(0, false), " Factory Dashboard ");
        assert_eq!(dashboard_title(0, true), " Factory Dashboard ⠋ ");
        assert_eq!(dashboard_title(1, true), " Factory Dashboard ⠙ ");
    }

    #[test]
    fn test_run_view_has_activity_from_status() {
        let executing =
            make_run_view_with_status("run-active", vec![AgentView::new("author")], "executing");
        let reviewing =
            make_run_view_with_status("run-review", vec![AgentView::new("author")], "reviewing");
        let complete =
            make_run_view_with_status("run-complete", vec![AgentView::new("author")], "complete");

        assert!(run_view_has_activity(&executing));
        assert!(run_view_has_activity(&reviewing));
        assert!(!run_view_has_activity(&complete));
    }

    #[test]
    fn test_run_view_has_activity_from_running_reviewer() {
        let mut reviewer = AgentView::new("tests");
        reviewer.status = "running".into();
        let view = make_run_view_with_status(
            "run-complete",
            vec![AgentView::new("author"), reviewer],
            "complete",
        );

        assert!(run_view_has_activity(&view));
    }

    #[test]
    fn test_header_reviewing_shows_progress() {
        let mut r1 = AgentView::new("tests");
        r1.status = "running".into();
        let mut r2 = AgentView::new("arch");
        r2.status = "pass".into();
        let mut r3 = AgentView::new("docs");
        r3.status = "running".into();
        let view = make_run_view("test-run", vec![AgentView::new("author"), r1, r2, r3]);
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
        let view =
            make_run_view_with_status("test-run", vec![AgentView::new("author"), r1], "complete");
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
        let view = make_run_view("test-run", vec![AgentView::new("author"), r1, r2]);
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
        fs::write(reviews_dir.join("transcript-behaviors.jsonl"), "{}").unwrap();

        let mut view = RunView {
            run: Run {
                id: "test-run".to_string(),
                dir: run_dir.clone(),
            },
            live_dir: run_dir.clone(),
            agents: vec![AgentView::new("author")],
            selected_agent: 0,
            agent_selection_touched: false,
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
    fn test_discover_agents_includes_review_without_transcript() {
        use std::fs;
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let run_dir = tmp.path().to_path_buf();
        let reviews_dir = run_dir.join("reviews");
        fs::create_dir_all(&reviews_dir).unwrap();
        fs::write(
            reviews_dir.join("review-architecture.md"),
            "Verdict: fail\n\nReviewer failed to launch.",
        )
        .unwrap();

        let mut view = RunView {
            run: Run {
                id: "test-run".to_string(),
                dir: run_dir.clone(),
            },
            live_dir: run_dir,
            agents: vec![AgentView::new("author")],
            selected_agent: 0,
            agent_selection_touched: false,
            scroll_offset: 0,
            auto_scroll: true,
            wrapped_total: 0,
            cached_status: "executing".to_string(),
        };

        view.discover_agents();

        assert_eq!(view.agents.len(), 2);
        assert_eq!(view.agents[1].name, "architecture");
        assert_eq!(view.agents[1].status, "fail");
        assert!(
            view.agents[1]
                .cached_lines
                .iter()
                .any(|line| line.contains("Reviewer failed to launch."))
        );
    }

    #[test]
    fn test_discover_agents_resets_archived_review_round_verdicts() {
        use std::fs;
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let run_dir = tmp.path().to_path_buf();
        let reviews_dir = run_dir.join("reviews");
        fs::create_dir_all(&reviews_dir).unwrap();

        fs::write(
            reviews_dir.join("transcript-behaviors.jsonl"),
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"old round content"}]}}"#,
        )
        .unwrap();
        fs::write(
            reviews_dir.join("review-behaviors.md"),
            "Verdict: pass\n\nOld round.",
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
            agent_selection_touched: false,
            scroll_offset: 0,
            auto_scroll: true,
            wrapped_total: 0,
            cached_status: "reviewing".to_string(),
        };

        view.discover_agents();
        assert_eq!(view.agents.len(), 2);
        assert_eq!(view.agents[1].name, "behaviors");
        assert_eq!(view.agents[1].status, "pass");
        view.poll();
        assert!(
            view.agents[1]
                .cached_lines
                .iter()
                .any(|line| line.contains("old round content"))
        );

        let archive_dir = reviews_dir.join("round-1");
        fs::create_dir_all(&archive_dir).unwrap();
        fs::rename(
            reviews_dir.join("review-behaviors.md"),
            archive_dir.join("review-behaviors.md"),
        )
        .unwrap();
        fs::rename(
            reviews_dir.join("transcript-behaviors.jsonl"),
            archive_dir.join("transcript-behaviors.jsonl"),
        )
        .unwrap();

        view.discover_agents();
        assert_eq!(view.agents.len(), 1);
        assert_eq!(view.agents[0].name, "author");

        fs::write(
            reviews_dir.join("transcript-behaviors.jsonl"),
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"new round content"}]}}"#,
        )
        .unwrap();
        view.poll();
        assert_eq!(view.agents.len(), 2);
        assert_eq!(view.agents[1].name, "behaviors");
        assert_eq!(view.agents[1].status, "running");
        assert!(
            view.agents[1]
                .cached_lines
                .iter()
                .any(|line| line.contains("new round content"))
        );
        assert!(
            !view.agents[1]
                .cached_lines
                .iter()
                .any(|line| line.contains("old round content"))
        );

        fs::write(
            reviews_dir.join("review-behaviors.md"),
            "Verdict: pass\n\nNew round.",
        )
        .unwrap();
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
        // "世界hello"
        let s = "\u{4e16}\u{754c}hello";
        // 世=2cols, 界=2cols, h=1col => first 4 cols = "世界"
        // 2 chars * 3 bytes each
        assert_eq!(split_at_width(s, 4), 6);
        // 5 cols: can't fit 世界h (2+2+1=5), can fit
        // 世界h = 6+1 bytes
        assert_eq!(split_at_width(s, 5), 7);
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
            work_status: WorkStatus {
                rows: Vec::new(),
                errors: Vec::new(),
            },
            show_work: false,
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
            attempts: Vec::new(),
            merge_candidates: Vec::new(),
        };
        item.add_initial_attempt(format!("{id}-attempt")).unwrap();
        WorkModelStore::new(project_root)
            .write_work_item(&item)
            .unwrap();
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
            work_status: WorkStatus {
                rows: Vec::new(),
                errors: Vec::new(),
            },
            show_work: false,
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
    fn test_work_view_renders_work_items_without_runs() {
        let mut app = App {
            runs: Vec::new(),
            work_status: WorkStatus {
                rows: vec![work_status::WorkItemStatus {
                    id: "work-1".to_string(),
                    title: "Build status view".to_string(),
                    attempt: "attempt-1 [planned]".to_string(),
                    task: "write:attempt-1-write [planned]".to_string(),
                    review: "-".to_string(),
                    merge_candidate: "-".to_string(),
                    merge: "-".to_string(),
                    action: "task-ready".to_string(),
                }],
                errors: Vec::new(),
            },
            show_work: true,
            selected_run: 0,
            search_root: PathBuf::from("/tmp"),
            should_quit: false,
            feed_height: 20,
            tick: 0,
            run_tab_offset: 0,
            copy_mode: false,
        };

        let backend = TestBackend::new(100, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw_ui(f, &mut app);
            })
            .unwrap();
        let text = buffer_text(terminal.backend().buffer());

        assert!(text.contains("Work Items"));
        assert!(text.contains("work-1"));
        assert!(text.contains("Build status view"));
        assert!(text.contains("task-ready"));
        assert!(text.contains("Attempt: attempt-1 [planned]"));
    }

    #[test]
    fn test_work_view_renders_empty_state_when_selected() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut app = App::new(tmp.path(), None).unwrap();
        app.show_work = true;

        let text = render_app_text(&mut app);

        assert!(text.contains("Work Items (0)"));
        assert!(text.contains("No Work Items found"));
        assert!(!text.contains("No runs in this project"));
    }

    #[test]
    fn test_work_view_counts_errors() {
        let mut app = App {
            runs: Vec::new(),
            work_status: WorkStatus {
                rows: vec![work_status::WorkItemStatus {
                    id: "work-1".to_string(),
                    title: "Build status view".to_string(),
                    attempt: "attempt-1 [planned]".to_string(),
                    task: "write:attempt-1-write [planned]".to_string(),
                    review: "-".to_string(),
                    merge_candidate: "-".to_string(),
                    merge: "-".to_string(),
                    action: "task-ready".to_string(),
                }],
                errors: vec![
                    "invalid Work Item file .factory/work/items/broken-work.json".to_string(),
                ],
            },
            show_work: true,
            selected_run: 0,
            search_root: PathBuf::from("/tmp"),
            should_quit: false,
            feed_height: 20,
            tick: 0,
            run_tab_offset: 0,
            copy_mode: false,
        };

        let text = render_app_text(&mut app);

        assert!(text.contains("Work Items: 1"));
        assert!(text.contains("Actionable: 1"));
        assert!(text.contains("Errors: 1"));
        assert!(text.contains("Work Item read errors"));
        assert!(text.contains(".factory/work/items/broken-work.json"));
    }

    #[test]
    fn test_app_poll_refreshes_work_items() {
        let tmp = tempfile::TempDir::new().unwrap();
        write_work_item(tmp.path(), "work-before", "Before poll");
        let mut app = App::new(tmp.path(), None).unwrap();

        assert!(app.show_work);
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
        assert!(app.show_work);
        let text = render_app_text(&mut app);
        assert!(text.contains("Work Items (2)"));
        assert!(text.contains("work-after - After poll"));
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
        let mut app = make_app_with_runs(&["run-0", "run-1", "run-2", "run-3", "run-4"], 4);
        app.run_tab_offset = 0;
        // With a narrow width, offset must advance to show run-4
        clamp_run_tab_offset(&mut app, 30);
        assert!(app.run_tab_offset > 0);
    }

    #[test]
    fn test_run_tabs_show_cached_live_status() {
        let mut app = make_app_with_runs(&["live-run"], 0);
        app.runs[0].cached_status = "executing".to_string();
        std::fs::create_dir_all(&app.runs[0].run.dir).unwrap();
        std::fs::write(app.runs[0].run.dir.join("status"), "planned").unwrap();

        let text = render_run_tabs_text(&mut app, 80);

        assert!(text.contains("live-run ["));
        assert!(text.contains("executing]"));
        assert!(!text.contains("[planned]"));
    }

    #[test]
    fn test_run_tabs_show_active_status_marker() {
        let mut app = make_app_with_runs(&["live-run"], 0);
        app.runs[0].cached_status = "executing".to_string();
        app.tick = 0;

        let text = render_run_tabs_text(&mut app, 80);

        assert!(text.contains("live-run [⠋ executing]"));
    }

    #[test]
    fn test_run_tabs_active_status_marker_advances() {
        let mut app = make_app_with_runs(&["live-run"], 0);
        app.runs[0].cached_status = "reviewing".to_string();
        app.tick = 0;
        let text_0 = render_run_tabs_text(&mut app, 80);

        app.tick = 1;
        let text_1 = render_run_tabs_text(&mut app, 80);

        assert!(text_0.contains("live-run [⠋ reviewing]"));
        assert!(text_1.contains("live-run [⠙ reviewing]"));
    }

    #[test]
    fn test_app_poll_sorts_actionable_runs_first() {
        let tmp = tempfile::TempDir::new().unwrap();
        let runs_dir = tmp.path().join(".factory/runs");
        make_filesystem_run(&runs_dir.join("a-complete"), "a-complete", "complete");
        make_filesystem_run(&runs_dir.join("b-executing"), "b-executing", "executing");
        make_filesystem_run(&runs_dir.join("c-failed"), "c-failed", "failed");

        let mut app = App::new(tmp.path(), Some("a-complete")).unwrap();
        app.poll();

        let ids: Vec<&str> = app.runs.iter().map(|v| v.run.id.as_str()).collect();
        assert_eq!(ids, vec!["b-executing", "c-failed", "a-complete"]);
        assert_eq!(app.runs[app.selected_run].run.id, "a-complete");
    }

    #[test]
    fn test_app_new_selects_run_with_live_active_status() {
        let tmp = tempfile::TempDir::new().unwrap();
        let runs_dir = tmp.path().join(".factory/runs");
        let worktree = tmp.path().join("worktree");
        let live_run_dir = worktree.join(".factory/runs/a-live");

        let source_live = runs_dir.join("a-live");
        make_filesystem_run(&source_live, "a-live", "complete");
        std::fs::write(
            source_live.join("worktree"),
            worktree.to_string_lossy().as_ref(),
        )
        .unwrap();
        make_filesystem_run(&live_run_dir, "a-live", "executing");
        make_filesystem_run(&runs_dir.join("b-planned"), "b-planned", "planned");

        let app = App::new(tmp.path(), None).unwrap();

        assert_eq!(app.runs[app.selected_run].run.id, "a-live");
        assert_eq!(app.runs[app.selected_run].cached_status, "executing");
    }

    #[test]
    fn test_app_poll_switches_to_live_status_when_worktree_run_appears() {
        let tmp = tempfile::TempDir::new().unwrap();
        let runs_dir = tmp.path().join(".factory/runs");
        let worktree = tmp.path().join("worktree");
        let source_live = runs_dir.join("a-live");
        let live_run_dir = worktree.join(".factory/runs/a-live");

        make_filesystem_run(&source_live, "a-live", "complete");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::write(
            source_live.join("worktree"),
            worktree.to_string_lossy().as_ref(),
        )
        .unwrap();

        let mut app = App::new(tmp.path(), Some("a-live")).unwrap();
        assert_eq!(app.runs[app.selected_run].cached_status, "complete");
        assert_eq!(app.runs[app.selected_run].live_dir, source_live);

        make_filesystem_run(&live_run_dir, "a-live", "needs-user");
        app.poll();

        assert_eq!(app.runs[app.selected_run].run.id, "a-live");
        assert_eq!(app.runs[app.selected_run].cached_status, "needs-user");
        assert_eq!(app.runs[app.selected_run].live_dir, live_run_dir);
    }

    #[test]
    fn test_app_new_prefers_actionable_run_over_cleaned_terminal_run() {
        let tmp = tempfile::TempDir::new().unwrap();
        let runs_dir = tmp.path().join(".factory/runs");
        let cleaned = runs_dir.join("a-cleaned");
        make_filesystem_run(&cleaned, "a-cleaned", "complete");
        std::fs::write(cleaned.join("cleaned.md"), "# Cleaned\n").unwrap();
        make_filesystem_run(&runs_dir.join("b-active"), "b-active", "executing");

        let app = App::new(tmp.path(), None).unwrap();

        assert_eq!(app.runs[app.selected_run].run.id, "b-active");
    }

    #[test]
    fn test_app_poll_removes_deleted_runs_and_selects_existing_run() {
        let tmp = tempfile::TempDir::new().unwrap();
        let runs_dir = tmp.path().join(".factory/runs");
        make_filesystem_run(&runs_dir.join("run-a"), "run-a", "complete");
        make_filesystem_run(&runs_dir.join("run-b"), "run-b", "executing");
        make_filesystem_run(&runs_dir.join("run-c"), "run-c", "complete");

        let mut app = App::new(tmp.path(), Some("run-b")).unwrap();

        std::fs::remove_dir_all(runs_dir.join("run-b")).unwrap();
        app.poll();

        let ids: Vec<&str> = app.runs.iter().map(|v| v.run.id.as_str()).collect();
        assert_eq!(ids, vec!["run-a", "run-c"]);
        assert_eq!(app.runs[app.selected_run].run.id, "run-c");
    }

    #[test]
    fn test_app_poll_renders_empty_state_after_all_runs_removed() {
        let tmp = tempfile::TempDir::new().unwrap();
        let runs_dir = tmp.path().join(".factory/runs");
        make_filesystem_run(&runs_dir.join("run-a"), "run-a", "executing");

        let mut app = App::new(tmp.path(), None).unwrap();

        std::fs::remove_dir_all(runs_dir.join("run-a")).unwrap();
        app.poll();

        assert!(app.runs.is_empty());
        assert_eq!(app.selected_run, 0);

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
                draw_help_bar(f, f.area(), false, false);
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
                draw_help_bar(f, f.area(), true, false);
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
