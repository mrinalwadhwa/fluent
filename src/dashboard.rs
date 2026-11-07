mod actions;
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

/// Launch the Work operator console with explicitly confirmed local actions.
pub fn run_dashboard(search_root: &Path) -> Result<()> {
    run_dashboard_with_projects(search_root, &[])
}

/// Launch the project Work view with additional local checkouts.
pub fn run_dashboard_with_projects(search_root: &Path, explicit: &[PathBuf]) -> Result<()> {
    let home = crate::scheduler_service::runtime_home(search_root)?;
    let mut app = loading_project_app();
    let config = match crate::dashboard_sources::load_sources(&home) {
        Ok(config) => config,
        Err(error) => {
            let error = format!("SSH sources: {error:#}");
            app.configuration_errors.push(error.clone());
            app.snapshot.errors.push(error);
            Vec::new()
        }
    };
    let mut remote = crate::dashboard_sources::RemoteSources::new(config, Default::default());
    app.snapshot.sources.extend(remote.snapshots());
    refresh_remote(&mut app, &mut remote);
    let mut session = DashboardTerminal::open()?;
    let mut local = LocalRefresh::new(search_root, explicit, &home);
    run_project_loop(&mut session, &mut app, &mut local, &mut remote)
}

fn loading_project_app() -> App {
    let mut app = App::new(snapshot::DashboardSnapshot {
        rows: Vec::new(),
        errors: Vec::new(),
        sources: Vec::new(),
        project_view: true,
    });
    app.local_read.pending = true;
    app.checking_sources = true;
    app
}

/// Keep terminal IO replaceable while tests drive the production event loop.
trait DashboardSession {
    fn size(&self) -> Result<(u16, u16)>;
    fn draw(&mut self, app: &App) -> Result<()>;
    fn next_event(&mut self, timeout: Duration) -> Result<Option<CEvent>>;
    fn fullscreen(&mut self, terminal: &mut crate::terminal_view::EmbeddedTerminal) -> Result<()>;
    fn run_action(&mut self, command: &actions::LocalCommand) -> Result<actions::CommandOutcome>;
    fn is_active(&self) -> bool;
    fn set_mouse_enabled(&mut self, enabled: bool) -> Result<()>;
}

impl DashboardSession for DashboardTerminal {
    fn size(&self) -> Result<(u16, u16)> {
        self.size()
    }
    fn draw(&mut self, app: &App) -> Result<()> {
        self.terminal.draw(|frame| render::draw(frame, app))?;
        Ok(())
    }
    fn next_event(&mut self, timeout: Duration) -> Result<Option<CEvent>> {
        if event::poll(timeout)? {
            Ok(Some(event::read()?))
        } else {
            Ok(None)
        }
    }
    fn fullscreen(&mut self, terminal: &mut crate::terminal_view::EmbeddedTerminal) -> Result<()> {
        let (columns, rows) = self.size()?;
        self.handoff(|| terminal.fullscreen(columns, rows))
            .and_then(|code| {
                anyhow::ensure!(code == 0, "terminal attachment exited with {code}");
                Ok(())
            })
    }
    fn run_action(&mut self, command: &actions::LocalCommand) -> Result<actions::CommandOutcome> {
        self.handoff(|| command.run())
    }
    fn is_active(&self) -> bool {
        self.is_active()
    }
    fn set_mouse_enabled(&mut self, enabled: bool) -> Result<()> {
        self.set_mouse_enabled(enabled)
    }
}

fn run_project_loop(
    session: &mut impl DashboardSession,
    app: &mut App,
    local: &mut LocalRefresh,
    remote: &mut crate::dashboard_sources::RemoteSources,
) -> Result<()> {
    let (width, height) = session.size()?;
    app.resize(width, height);
    // Draw a usable loading frame before even requesting local discovery.
    if app.take_dirty() {
        session.draw(app)?;
    }
    local.request(app, false);
    let mut next_poll = Instant::now() + DATA_POLL_INTERVAL;
    let mut embedded: Option<crate::terminal_view::EmbeddedTerminal> = None;

    loop {
        local.poll(app);
        let completed = remote.poll_results();
        if !completed.is_empty() {
            apply_remote_results(app, completed);
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
        app.update_read_display(Instant::now());
        if app.take_dirty() {
            session.draw(app)?;
        }
        let timeout = next_poll
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(33));
        if let Some(event) = session.next_event(timeout)? {
            let native = embedded.as_ref().is_some_and(|terminal| terminal.focused())
                && matches!(event, CEvent::Key(_) | CEvent::Paste(_) | CEvent::Mouse(_));
            if native {
                let terminal = embedded.as_mut().unwrap();
                let event = terminal_input_event(app, event);
                if let Some(event) = event {
                    if let Err(error) = terminal.send(event) {
                        app.refresh_failed(format!("{error:#}"));
                    }
                    app.set_terminal(terminal.display());
                }
            } else {
                match event {
                    CEvent::Key(key) if key.kind != crossterm::event::KeyEventKind::Release => {
                        let effect = handle_terminal_key(app, &mut embedded, key, |terminal| {
                            session.fullscreen(terminal)
                        });
                        if !session.is_active() {
                            // Full-screen restoration exhausted its display
                            // budget. Exit without another draw or diagnostic
                            // write to the unread PTY; workers remain owned.
                            break;
                        }
                        if effect == Effect::FullscreenTerminal {
                            let (width, height) = session.size()?;
                            resize_terminal(app, embedded.as_mut(), width, height);
                            local.request(app, true);
                            refresh_remote(app, remote);
                            next_poll = Instant::now() + DATA_POLL_INTERVAL;
                        }
                        if effect == Effect::PrepareAction {
                            prepare_dashboard_action(app);
                        }
                        if matches!(effect, Effect::PrepareAction | Effect::RunAction) {
                            run_dashboard_action(session, app, local)?;
                            if !session.is_active() {
                                break;
                            }
                            let (width, height) = session.size()?;
                            resize_terminal(app, embedded.as_mut(), width, height);
                            next_poll = Instant::now() + DATA_POLL_INTERVAL;
                        }
                        if effect == Effect::CopyAction {
                            if let Some(menu) = &app.action_menu {
                                let result = actions::copy_command(menu.current());
                                if let Effect::ToggleMouse(enabled) = app.complete_copy(result) {
                                    session.set_mouse_enabled(enabled)?;
                                }
                            }
                        }
                        if let Effect::ToggleMouse(enabled) = effect {
                            session.set_mouse_enabled(enabled)?;
                        }
                        if matches!(effect, Effect::Refresh) {
                            local.request(app, true);
                            refresh_remote(app, remote);
                            next_poll = Instant::now() + DATA_POLL_INTERVAL;
                        }
                    }
                    CEvent::Resize(width, height) => {
                        resize_terminal(app, embedded.as_mut(), width, height);
                    }
                    _ => {}
                }
            }
        }
        if poll_due(Instant::now(), next_poll) {
            local.request(app, false);
            refresh_remote(app, remote);
            next_poll = Instant::now() + DATA_POLL_INTERVAL;
        }
        if app.should_quit() {
            break;
        }
    }
    Ok(())
}

/// Prepare a concrete confirmation from a fresh read; never execute guidance.
fn prepare_dashboard_action(app: &mut App) {
    let result = (|| {
        let action = app
            .action_menu
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No action selected"))?
            .current()
            .clone();
        validate_action_selection(app, &action)?;
        actions::PreparedAction::prepare(action)
    })();
    app.detail_scroll = 0;
    app.action_message = None;
    match result {
        Ok(action) if action.action.kind == actions::ActionKind::CandidateShow => {
            app.confirmed_action = Some(action)
        }
        Ok(action) => app.pending_action = Some(action),
        Err(error) => app.action_message = Some(format!("Action canceled: {error:#}")),
    }
    app.dirty = true;
}

fn validate_action_selection(app: &App, action: &actions::OfferedAction) -> Result<()> {
    let key = app
        .selected_key
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Work selection changed"))?;
    anyhow::ensure!(
        key.source_id == action.source_id && key.work_item_id == action.work_item_id,
        "Work selection changed; choose the action again"
    );
    let (source, work) = project::find_work(&app.snapshot.sources, key)
        .ok_or_else(|| anyhow::anyhow!("Selected source or Work Item disappeared"))?;
    anyhow::ensure!(
        !app.source_warning(source),
        "Selected source is stale; refresh before choosing an action"
    );
    anyhow::ensure!(
        actions::offered(source, work).contains(action),
        "Source, Work Item, or action changed; choose it again"
    );
    Ok(())
}

/// Consume one confirmation, restore the dashboard, then request a post-command read.
fn run_dashboard_action(
    session: &mut impl DashboardSession,
    app: &mut App,
    local: &mut LocalRefresh,
) -> Result<()> {
    let Some(action) = app.take_confirmed_action() else {
        return Ok(());
    };
    let result = validate_action_selection(app, &action.action)
        .and_then(|()| action.validate())
        .and_then(|command| session.run_action(&command));
    app.action_menu = None;
    app.pending_action = None;
    app.detail_scroll = 0;
    app.action_message = Some(match result {
        Ok(outcome) => format!(
            "{} {outcome}; refreshing source",
            action.action.kind.label()
        ),
        Err(error) => format!(
            "{} did not complete: {error:#}; refreshing source",
            action.action.kind.label()
        ),
    });
    app.dirty = true;
    if !session.is_active() {
        return Ok(());
    }
    let (width, height) = session.size()?;
    app.resize(width, height);
    // A read started before the command cannot publish its older snapshot after
    // the command. Dropping its handle detaches the read-only worker.
    local.pending.take();
    local.manual_pending = false;
    local.request(app, true);
    Ok(())
}

fn refresh_remote(app: &mut App, remote: &mut crate::dashboard_sources::RemoteSources) {
    remote.refresh();
    for id in remote.pending_ids() {
        app.source_reads.entry(id).or_default().pending = true;
    }
    app.dirty = true;
}

/// Apply remote completions to the current local and interaction state.
fn apply_remote_results(app: &mut App, sources: Vec<project::SourceSnapshot>) {
    let mut snapshot = app.snapshot.clone();
    // Replace diagnostics only for sources included in this completion.
    for source in sources {
        app.record_source_read(&source);
        if let Some(index) = snapshot.sources.iter().position(|old| old.id == source.id) {
            let old_errors = project_diagnostics(&snapshot.sources[index..=index]);
            snapshot.errors.retain(|error| !old_errors.contains(error));
            snapshot.sources[index] = source.clone();
        } else {
            snapshot.sources.push(source.clone());
        }
        snapshot.errors.extend(project_diagnostics(&[source]));
    }
    app.refresh(snapshot);
}

/// Apply navigation and terminal actions before the event loop refreshes data.
fn handle_terminal_key(
    app: &mut App,
    embedded: &mut Option<crate::terminal_view::EmbeddedTerminal>,
    key: crossterm::event::KeyEvent,
    fullscreen: impl FnOnce(&mut crate::terminal_view::EmbeddedTerminal) -> Result<()>,
) -> Effect {
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
    if effect == Effect::OpenTerminal {
        let same_view = app
            .selected_worker
            .as_ref()
            .and_then(|key| project::find_worker(&app.snapshot.sources, key))
            .and_then(|worker| worker.terminal.as_ref())
            .is_some_and(|descriptor| {
                embedded
                    .as_ref()
                    .is_some_and(|terminal| terminal.descriptor() == descriptor)
            });
        let result = if same_view {
            embedded.as_mut().unwrap().reconnect()
        } else {
            // Replace the retained invocation only after the exact new target opens.
            open_embedded(app).map(|terminal| *embedded = Some(terminal))
        };
        match result {
            Ok(()) => {
                app.set_terminal(embedded.as_ref().unwrap().display());
            }
            Err(error) => {
                app.show_details = true;
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
            let identity = terminal.identity();
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
        .selected_worker
        .as_ref()
        .context("open Workers in details and select an agent worker, then press t")?;
    let worker = project::find_worker(&app.snapshot.sources, key)
        .context("open Workers in details and select an agent worker")?;
    let descriptor = worker
        .terminal
        .as_ref()
        .context(worker.unavailable_reason.clone())?;
    anyhow::ensure!(
        descriptor.can_observe,
        "{}",
        descriptor
            .unavailable_reason
            .as_deref()
            .unwrap_or(&worker.unavailable_reason)
    );
    let source = app
        .snapshot
        .sources
        .iter()
        .find(|source| source.id == key.source_id)
        .context("source is unavailable")?;
    if source.id.starts_with("ssh:") {
        let home = crate::scheduler_service::runtime_home(&source.path)?;
        let configured = crate::dashboard_sources::load_sources(&home)?
            .into_iter()
            .find(|configured| configured.id() == source.id)
            .context("SSH source is no longer configured")?;
        anyhow::ensure!(
            configured.host == source.host && configured.project_root == source.path,
            "SSH source configuration changed; reopen the dashboard"
        );
        let identity = format!(
            "{} ({}) / {}",
            configured.name,
            configured.host,
            crate::terminal_view::identity(descriptor)
        );
        let (columns, rows) = layout::terminal_size(app.size, &identity);
        return crate::terminal_view::EmbeddedTerminal::observe_ssh(
            Default::default(),
            configured,
            descriptor.clone(),
            columns,
            rows,
        );
    }
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
            let prefix = if source.id.starts_with("ssh:") {
                format!("{} ({}): ", source.id, source.path.display())
            } else {
                format!("{}: ", source.path.display())
            };
            source
                .diagnostics
                .iter()
                .map({
                    let prefix = prefix.clone();
                    move |diagnostic| format!("{prefix}{diagnostic}")
                })
                .chain(
                    source
                        .stale
                        .iter()
                        .map(move |reason| format!("{prefix}STALE: {reason}")),
                )
        })
        .collect()
}

/// Keep local discovery and Work reads off the input and drawing thread.
struct LocalRefresh {
    root: PathBuf,
    explicit: Vec<PathBuf>,
    home: PathBuf,
    pending: Option<std::thread::JoinHandle<snapshot::DashboardSnapshot>>,
    manual_pending: bool,
}

impl LocalRefresh {
    fn new(root: &Path, explicit: &[PathBuf], home: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            explicit: explicit.to_vec(),
            home: home.to_path_buf(),
            pending: None,
            manual_pending: false,
        }
    }

    fn request(&mut self, app: &mut App, manual: bool) {
        if self.pending.is_some() {
            self.manual_pending |= manual;
            app.checking_sources |= manual;
            // Recheck overdue snapshots on poll ticks even while a read is blocked.
            app.dirty = true;
            return;
        }
        let root = self.root.clone();
        let explicit = self.explicit.clone();
        let home = self.home.clone();
        let previous = app
            .snapshot
            .sources
            .iter()
            .filter(|source| !source.id.starts_with("ssh:"))
            .cloned()
            .collect::<Vec<_>>();
        self.start(app, move || {
            refresh_projects(&root, &explicit, &home, &previous)
        });
        app.checking_sources |= manual;
    }

    fn start(
        &mut self,
        app: &mut App,
        read: impl FnOnce() -> snapshot::DashboardSnapshot + Send + 'static,
    ) {
        app.begin_local_read();
        match std::thread::Builder::new()
            .name("local-snapshot".into())
            .spawn(read)
        {
            Ok(handle) => self.pending = Some(handle),
            Err(error) => app.local_read_failed(format!("Local snapshot reader: {error}")),
        }
    }

    fn poll(&mut self, app: &mut App) -> bool {
        if !self
            .pending
            .as_ref()
            .is_some_and(|handle| handle.is_finished())
        {
            return false;
        }
        // Only join a finished read; quitting also never waits for a local read.
        match self.pending.take().unwrap().join() {
            Ok(mut snapshot) => {
                let error = snapshot
                    .sources
                    .iter()
                    .find_map(|source| source.stale.as_deref());
                app.local_read.complete(error);
                app.dirty = true;
                for source in &snapshot.sources {
                    app.record_source_read(source);
                }
                snapshot.errors.extend(app.configuration_errors.clone());
                let remote = app
                    .snapshot
                    .sources
                    .iter()
                    .filter(|source| source.id.starts_with("ssh:"))
                    .cloned()
                    .collect::<Vec<_>>();
                snapshot.errors.extend(project_diagnostics(&remote));
                snapshot.sources.extend(remote);
                app.refresh(snapshot);
            }
            Err(_) => app.local_read_failed("Local snapshot reader stopped".into()),
        }
        if std::mem::take(&mut self.manual_pending) {
            self.request(app, true);
        }
        true
    }
}

fn refresh_projects(
    root: &Path,
    explicit: &[PathBuf],
    home: &Path,
    previous: &[project::SourceSnapshot],
) -> snapshot::DashboardSnapshot {
    let (mut paths, errors) = project::discover_paths(root, explicit, home);
    // Keep requested paths represented even when initial canonicalization fails.
    paths.extend(std::iter::once(root.to_path_buf()).chain(explicit.iter().cloned()));
    paths.extend(previous.iter().map(|source| source.path.clone()));
    paths = paths
        .into_iter()
        .map(|path| path.canonicalize().unwrap_or(path))
        .collect();
    paths.sort();
    paths.dedup();
    let mut next = snapshot::DashboardSnapshot {
        rows: Vec::new(),
        sources: project::refresh_sources(previous, &paths),
        errors,
        project_view: true,
    };
    next.errors.extend(project_diagnostics(&next.sources));
    next
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
