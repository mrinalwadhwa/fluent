mod app;
mod layout;
mod model;
pub mod project;
mod render;
mod snapshot;
pub mod terminal;
#[cfg(feature = "test-support")]
pub mod test_support;
mod transitions;

use anyhow::Result;
use app::{App, Effect};
use crossterm::event::{self, Event as CEvent};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use terminal::DashboardTerminal;

use crate::work_status;

const DATA_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Launch the read-only Work operator console.
pub fn run_dashboard(search_root: &Path) -> Result<()> {
    run_dashboard_with_projects(search_root, &[])
}

/// Launch the project Work view with additional local checkouts.
pub fn run_dashboard_with_projects(search_root: &Path, explicit: &[PathBuf]) -> Result<()> {
    let home = crate::scheduler_service::runtime_home(search_root)?;
    let (paths, discovery_errors) = project::discover_paths(search_root, explicit, &home);
    let mut snapshot = load_project_snapshot(&paths, discovery_errors);
    let config = match crate::dashboard_sources::load_sources(&home) {
        Ok(config) => config,
        Err(error) => {
            snapshot.errors.push(format!("SSH sources: {error:#}"));
            Vec::new()
        }
    };
    let mut remote = crate::dashboard_sources::RemoteSources::new(config, Default::default());
    remote.refresh();
    snapshot.sources.extend(remote.snapshots());
    let mut session = DashboardTerminal::open()?;
    let mut app = App::new(snapshot);
    let size = session.terminal.size()?;
    app.resize(size.width, size.height);
    let mut next_poll = Instant::now() + DATA_POLL_INTERVAL;
    let mut embedded: Option<crate::terminal_view::EmbeddedTerminal> = None;

    loop {
        if remote.poll() {
            let mut snapshot = app.snapshot.clone();
            snapshot.sources.retain(|s| !s.id.starts_with("ssh:"));
            snapshot.sources.extend(remote.snapshots());
            snapshot.errors = project_diagnostics(&snapshot.sources);
            app.refresh(snapshot);
        }
        if let Some(terminal) = &mut embedded {
            if terminal.focused() && !app.terminal_visible() {
                terminal.release_focus();
                app.set_terminal(terminal.display());
            }
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
            let event = event::read()?;
            let native = embedded.as_ref().is_some_and(|terminal| terminal.focused())
                && matches!(event, CEvent::Key(_) | CEvent::Paste(_) | CEvent::Mouse(_));
            if native {
                let terminal = embedded.as_mut().unwrap();
                let event = terminal_input_event(&app, event);
                if let Some(event) = event {
                    if let Err(error) = terminal.send(event) {
                        app.refresh_failed(format!("{error:#}"));
                    }
                    app.set_terminal(terminal.display());
                }
            } else {
                match event {
                    CEvent::Key(key) if key.kind != crossterm::event::KeyEventKind::Release => {
                        let effect =
                            handle_terminal_key(&mut app, &mut embedded, key, |terminal| {
                                let (columns, rows) = session.size()?;
                                session
                                    .handoff(|| terminal.fullscreen(columns, rows))
                                    .and_then(|code| {
                                        anyhow::ensure!(
                                            code == 0,
                                            "terminal attachment exited with {code}"
                                        );
                                        Ok(())
                                    })
                            });
                        if effect == Effect::FullscreenTerminal {
                            let (width, height) = session.size()?;
                            resize_terminal(&mut app, embedded.as_mut(), width, height);
                            refresh_projects(search_root, explicit, &home, &mut app);
                            remote.refresh();
                            next_poll = Instant::now() + DATA_POLL_INTERVAL;
                        }
                        if let Effect::ToggleMouse(enabled) = effect {
                            session.set_mouse_enabled(enabled)?;
                        }
                        if matches!(effect, Effect::Refresh) {
                            refresh_projects(search_root, explicit, &home, &mut app);
                            remote.refresh();
                            next_poll = Instant::now() + DATA_POLL_INTERVAL;
                        }
                    }
                    CEvent::Resize(width, height) => {
                        resize_terminal(&mut app, embedded.as_mut(), width, height);
                    }
                    _ => {}
                }
            }
        }
        if poll_due(Instant::now(), next_poll) {
            refresh_projects(search_root, explicit, &home, &mut app);
            remote.refresh();
            next_poll = Instant::now() + DATA_POLL_INTERVAL;
        }
        if app.should_quit() {
            break;
        }
    }
    Ok(())
}

/// Apply navigation and terminal actions before the event loop refreshes data.
fn handle_terminal_key(
    app: &mut App,
    embedded: &mut Option<crate::terminal_view::EmbeddedTerminal>,
    key: crossterm::event::KeyEvent,
    fullscreen: impl FnOnce(&mut crate::terminal_view::EmbeddedTerminal) -> Result<()>,
) -> Effect {
    let previous = app.selected_key.clone();
    let effect = app.handle_key(key.code, key.modifiers);
    if let Some(terminal) = embedded.as_mut() {
        let result = match effect {
            Effect::ControlTerminal => terminal.acquire_control(),
            Effect::ObserveTerminal => terminal.observe_only(),
            Effect::LiteralEscape => terminal.send_literal_escape(),
            Effect::FullscreenTerminal => fullscreen(terminal),
            _ => Ok(()),
        };
        if let Err(error) = result {
            app.refresh_failed(format!("{error:#}"));
        }
        app.set_terminal(terminal.display());
    }
    if effect == Effect::OpenTerminal || embedded.is_some() && previous != app.selected_key {
        let same_view = previous == app.selected_key && embedded.is_some();
        let result = if same_view {
            embedded.as_mut().unwrap().reconnect()
        } else {
            // An explicit selection releases the old client's authority.
            embedded.take();
            app.terminal = None;
            open_embedded(app).map(|terminal| *embedded = Some(terminal))
        };
        match result {
            Ok(()) => {
                app.set_terminal(embedded.as_ref().unwrap().display());
            }
            Err(error) => {
                app.refresh_failed(format!("Terminal unavailable: {error:#}"));
            }
        }
    }
    effect
}

fn terminal_input_event(app: &App, event: CEvent) -> Option<CEvent> {
    if !app.terminal_visible() {
        return None;
    }
    match event {
        CEvent::Mouse(mut mouse) => {
            let area = layout::terminal_content(app.size, &app.terminal.as_ref()?.identity);
            if !area.contains((mouse.column, mouse.row).into()) {
                return None;
            }
            mouse.column -= area.x;
            mouse.row -= area.y;
            Some(CEvent::Mouse(mouse))
        }
        CEvent::Key(_) | CEvent::Paste(_) => Some(event),
        _ => None,
    }
}

fn resize_terminal(
    app: &mut App,
    terminal: Option<&mut crate::terminal_view::EmbeddedTerminal>,
    width: u16,
    height: u16,
) {
    app.resize(width, height);
    if let Some(terminal) = terminal {
        if !app.terminal_visible() {
            terminal.release_focus();
        }
        if app.terminal_fits() {
            let identity = crate::terminal_view::identity(terminal.descriptor());
            let (columns, rows) = layout::terminal_size(app.size, &identity);
            if let Err(error) = terminal.resize(columns, rows) {
                app.refresh_failed(format!("{error:#}"));
            }
        }
        app.set_terminal(terminal.display());
    }
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
    let (columns, rows) =
        layout::terminal_size(app.size, &crate::terminal_view::identity(descriptor));
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
            .filter(|source| !source.id.starts_with("ssh:"))
            .map(|source| source.path.clone()),
    );
    paths.sort();
    paths.dedup();
    let mut next = app.snapshot.clone();
    let remote = next
        .sources
        .iter()
        .filter(|s| s.id.starts_with("ssh:"))
        .cloned()
        .collect::<Vec<_>>();
    let local = next
        .sources
        .iter()
        .filter(|s| !s.id.starts_with("ssh:"))
        .cloned()
        .collect::<Vec<_>>();
    next.sources = project::refresh_sources(&local, &paths);
    next.sources.extend(remote);
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
