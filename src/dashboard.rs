mod app;
mod layout;
pub mod project;
mod render;
mod snapshot;
mod transitions;

use anyhow::Result;
use app::{App, Effect};
use crossterm::event::{self, Event as CEvent};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::{Terminal, backend::CrosstermBackend};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::work_status;

const DATA_POLL_INTERVAL: Duration = Duration::from_secs(2);

struct TerminalSession {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
    cleanup: TerminalCleanup,
}

struct TerminalCleanup {
    raw: bool,
    alternate: bool,
    mouse: bool,
}
impl TerminalCleanup {
    fn restore(&mut self) {
        let mut stdout = io::stdout();
        if self.mouse {
            let _ = crossterm::execute!(stdout, crossterm::event::DisableMouseCapture);
            self.mouse = false;
        }
        if self.alternate {
            let _ = crossterm::execute!(stdout, LeaveAlternateScreen);
            self.alternate = false;
        }
        if self.raw {
            let _ = disable_raw_mode();
            self.raw = false;
        }
    }
}
impl Drop for TerminalCleanup {
    fn drop(&mut self) {
        self.restore();
    }
}

impl TerminalSession {
    fn open() -> Result<Self> {
        let mut cleanup = TerminalCleanup {
            raw: false,
            alternate: false,
            mouse: false,
        };
        enable_raw_mode()?;
        cleanup.raw = true;
        let mut stdout = io::stdout();
        cleanup.alternate = true;
        cleanup.mouse = true;
        if let Err(error) = crossterm::execute!(
            stdout,
            EnterAlternateScreen,
            crossterm::event::EnableMouseCapture
        ) {
            return Err(error.into());
        }
        Ok(Self {
            terminal: Terminal::new(CrosstermBackend::new(stdout))?,
            cleanup,
        })
    }

    fn set_mouse_enabled(&mut self, enabled: bool) -> Result<()> {
        if self.cleanup.mouse != enabled {
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
            self.cleanup.mouse = enabled;
        }
        Ok(())
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = self.terminal.show_cursor();
    }
}

/// Launch the read-only Work operator console.
pub fn run_dashboard(search_root: &Path) -> Result<()> {
    run_dashboard_with_projects(search_root, &[])
}

/// Launch the project Work view with additional local checkouts.
pub fn run_dashboard_with_projects(search_root: &Path, explicit: &[PathBuf]) -> Result<()> {
    let home = crate::scheduler_service::runtime_home(search_root)?;
    let (paths, discovery_errors) = project::discover_paths(search_root, explicit, &home);
    let snapshot = load_project_snapshot(&paths, discovery_errors);
    let mut session = TerminalSession::open()?;
    let mut app = App::new(snapshot);
    let size = session.terminal.size()?;
    app.resize(size.width, size.height);
    let mut next_poll = Instant::now() + DATA_POLL_INTERVAL;
    let mut embedded: Option<crate::terminal_view::EmbeddedTerminal> = None;

    loop {
        if let Some(terminal) = &mut embedded {
            if terminal.refresh() {
                app.set_terminal(terminal.display());
            }
        }
        if app.take_dirty() {
            session.terminal.draw(|frame| render::draw(frame, &app))?;
        }
        let timeout = next_poll
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(33));
        if event::poll(timeout)? {
            match event::read()? {
                CEvent::Key(key) => {
                    let previous = app.selected_key.clone();
                    let effect = app.handle_key(key.code, key.modifiers);
                    if effect == Effect::OpenTerminal
                        || embedded.is_some() && previous != app.selected_key
                    {
                        match open_embedded(&app) {
                            Ok(terminal) => {
                                app.set_terminal(terminal.display());
                                embedded = Some(terminal);
                            }
                            Err(error) => {
                                app.refresh_failed(format!("Terminal unavailable: {error:#}"));
                            }
                        }
                    }
                    if let Effect::ToggleMouse(enabled) = effect {
                        session.set_mouse_enabled(enabled)?;
                    }
                    if matches!(effect, Effect::Refresh) {
                        refresh_projects(search_root, explicit, &home, &mut app);
                        next_poll = Instant::now() + DATA_POLL_INTERVAL;
                    }
                }
                CEvent::Resize(width, height) => app.resize(width, height),
                _ => {}
            }
        }
        if poll_due(Instant::now(), next_poll) {
            refresh_projects(search_root, explicit, &home, &mut app);
            next_poll = Instant::now() + DATA_POLL_INTERVAL;
        }
        if app.should_quit() {
            break;
        }
    }
    Ok(())
}

fn open_embedded(app: &App) -> Result<crate::terminal_view::EmbeddedTerminal> {
    use anyhow::Context;
    let key = app
        .selected_key
        .as_ref()
        .context("select an agent worker, then press t")?;
    let worker = project::find_worker(&app.snapshot.sources, key)
        .context("expand the Work Item and select an agent worker")?;
    let descriptor = worker
        .terminal
        .as_ref()
        .context(worker.unavailable_reason.clone())?;
    let source = app
        .snapshot
        .sources
        .iter()
        .find(|source| source.id == key.source_id)
        .context("source is unavailable")?;
    let attachment = crate::terminal_sessions::resolve(&source.path, &descriptor.session_id)?;
    anyhow::ensure!(
        attachment.descriptor() == descriptor,
        "invocation changed; refresh and select it again"
    );
    let (columns, rows) = layout::terminal_size(app.size);
    crate::terminal_view::EmbeddedTerminal::observe(attachment, columns, rows)
}

fn load_project_snapshot(
    paths: &[PathBuf],
    mut errors: Vec<String>,
) -> snapshot::DashboardSnapshot {
    let mut sources = Vec::new();
    for path in paths {
        match project::SourceSnapshot::load(path) {
            Ok(source) => sources.push(source),
            Err(error) => errors.push(format!("{}: {error:#}", path.display())),
        }
    }
    errors.extend(project_diagnostics(&sources));
    snapshot::DashboardSnapshot {
        rows: Vec::new(),
        errors,
        sources,
        project_view: true,
    }
}

fn project_diagnostics(sources: &[project::SourceSnapshot]) -> Vec<String> {
    sources
        .iter()
        .flat_map(|source| {
            let prefix = format!("{}: ", source.path.display());
            source
                .diagnostics
                .iter()
                .map(move |diagnostic| format!("{prefix}{diagnostic}"))
                .chain(
                    source
                        .stale
                        .iter()
                        .map(move |reason| format!("{}: STALE: {reason}", source.path.display())),
                )
        })
        .collect()
}

fn refresh_projects(root: &Path, explicit: &[PathBuf], home: &Path, app: &mut App) {
    let (mut paths, errors) = project::discover_paths(root, explicit, home);
    paths.extend(
        app.snapshot
            .sources
            .iter()
            .map(|source| source.path.clone()),
    );
    paths.sort();
    paths.dedup();
    let mut next = app.snapshot.clone();
    next.sources = project::refresh_sources(&next.sources, &paths);
    next.errors = errors;
    next.errors.extend(project_diagnostics(&next.sources));
    app.refresh(next);
}

fn poll_due(now: Instant, deadline: Instant) -> bool {
    now >= deadline
}

fn refresh(root: &Path, app: &mut App) {
    match load_snapshot(root) {
        Ok(snapshot) => app.refresh(snapshot),
        Err(error) => app.refresh_failed(error.to_string()),
    }
}

fn load_snapshot(root: &Path) -> Result<snapshot::DashboardSnapshot> {
    let work = crate::work_model::WorkModelStore::new(root).snapshot_work_items()?;
    let transitions = transitions::latest_transitions(&work.items);
    let status = work_status::summarize_work_snapshot(work, root);
    Ok(snapshot::DashboardSnapshot::from_status_and_transitions(
        status,
        transitions,
    ))
}

#[cfg(test)]
mod tests;
