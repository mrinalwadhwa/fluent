//! Drive the actual navigation and source refresh paths during live display IO.
use super::*;
use anyhow::ensure;
use crossterm::event::{KeyCode, KeyModifiers};

fn assert_visible_control(app: &App) -> Result<()> {
    use ratatui::{Terminal, backend::TestBackend};
    let mut view = Terminal::new(TestBackend::new(app.size.0, app.size.1))?;
    view.draw(|frame| render::draw(frame, app))?;
    let buffer = view.backend().buffer();
    let rendered: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
    // Inspect the terminal header, not the dashboard's always-visible shortcuts.
    ensure!(
        rendered.contains("Control · Ctrl-] navigate"),
        "Control state hidden: {rendered}"
    );
    let identity = &app.terminal.as_ref().unwrap().identity;
    let content = layout::terminal_content(app.size, identity);
    let header: String = (6..content.y)
        .flat_map(|y| (content.x..content.right()).map(move |x| buffer[(x, y)].symbol()))
        .collect();
    let compact = |text: &str| {
        text.chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
    };
    ensure!(
        compact(&header) == compact(identity),
        "invocation identity hidden: {rendered}"
    );
    Ok(())
}

/// Keep one dashboard alive while tests drive production selection and actions.
pub struct TerminalDashboard {
    app: App,
    embedded: Option<crate::terminal_view::EmbeddedTerminal>,
}

impl TerminalDashboard {
    pub fn open(root: &Path) -> Result<Self> {
        let mut app = App::new(load_project_snapshot(&[root.to_owned()], Vec::new()));
        app.resize(80, 24);
        let mut dashboard = Self {
            app,
            embedded: None,
        };
        for code in [KeyCode::Enter, KeyCode::Down, KeyCode::Char('t')] {
            dashboard.key(CEvent::Key(crossterm::event::KeyEvent::new(
                code,
                KeyModifiers::NONE,
            )))?;
        }
        ensure!(
            dashboard.embedded.is_some(),
            "terminal did not open: {:?}",
            dashboard.app.stale_error
        );
        ensure!(
            dashboard
                .app
                .project_keys()
                .iter()
                .filter(|key| key.task_id.is_some())
                .count()
                == 2,
            "fixture must publish two selectable workers"
        );
        Ok(dashboard)
    }

    pub fn terminal(&mut self) -> &mut crate::terminal_view::EmbeddedTerminal {
        let selected = self.app.selected_key.as_ref().unwrap();
        let worker = project::find_worker(&self.app.snapshot.sources, selected).unwrap();
        let terminal = self
            .embedded
            .as_mut()
            .expect("selected worker has no terminal");
        assert_eq!(Some(terminal.descriptor()), worker.terminal.as_ref());
        terminal
    }

    pub fn key(&mut self, event: CEvent) -> Result<()> {
        if let Some(terminal) = &self.embedded {
            ensure!(!terminal.focused(), "terminal still owns native input");
            self.app.set_terminal(terminal.display());
        }
        let CEvent::Key(key) = event else {
            anyhow::bail!("expected a dashboard key");
        };
        handle_terminal_key(&mut self.app, &mut self.embedded, key, |_| {
            anyhow::bail!("full-screen attachment requires the handoff fixture")
        });
        Ok(())
    }

    /// Enter Control after opening the terminal and returning to the narrow list.
    pub fn control_from_narrow_list(&mut self) -> Result<()> {
        self.key(CEvent::Key(crossterm::event::KeyEvent::new(
            KeyCode::Esc,
            KeyModifiers::NONE,
        )))?;
        ensure!(self.app.pane == app::Pane::List);
        self.key(CEvent::Key(crossterm::event::KeyEvent::new(
            KeyCode::Char('i'),
            KeyModifiers::NONE,
        )))?;
        self.assert_visible_control()
    }

    pub fn assert_visible_control(&self) -> Result<()> {
        assert_visible_control(&self.app)
    }

    /// Exercise the production resize and input gates while the worker captures bytes.
    pub fn resize_control_and_resume(&mut self) -> Result<()> {
        ensure!(self.terminal().focused());
        for (width, height) in [(59, 24), (80, 14)] {
            resize_terminal(&mut self.app, self.embedded.as_mut(), width, height);
            let terminal = self.embedded.as_mut().unwrap();
            ensure!(!terminal.focused(), "resize left hidden Control focused");
            for event in [
                CEvent::Key(crossterm::event::KeyEvent::new(
                    KeyCode::Char('x'),
                    KeyModifiers::NONE,
                )),
                CEvent::Paste("hidden".into()),
            ] {
                ensure!(terminal_input_event(&self.app, event.clone()).is_none());
                ensure!(
                    terminal.send(event).is_err(),
                    "hidden input reached the worker"
                );
            }
            for key in ['i', ']'] {
                ensure!(
                    self.app.handle_key(KeyCode::Char(key), KeyModifiers::NONE) == Effect::None
                );
            }
            resize_terminal(&mut self.app, self.embedded.as_mut(), 80, 24);
            ensure!(
                !self.terminal().focused(),
                "resize silently restored input focus"
            );
            // This resume action contributes one literal Ctrl-] to the byte assertion.
            self.key(CEvent::Key(crossterm::event::KeyEvent::new(
                KeyCode::Char(']'),
                KeyModifiers::NONE,
            )))?;
            self.assert_visible_control()?;
        }
        Ok(())
    }
}

pub fn navigate_and_refresh(
    root: &Path,
    terminal: &mut crate::terminal_view::EmbeddedTerminal,
) -> Result<()> {
    let paths = vec![root.to_owned()];
    let mut app = App::new(load_project_snapshot(&paths, Vec::new()));
    app.filter = project::Filter::All;
    let keys = app.project_keys();
    ensure!(keys.len() >= 2, "fixture must publish two Work items");
    app.select_key(keys[0].clone());
    let start = Instant::now();
    let mut frames = 0;
    for _ in 0..30 {
        if terminal.refresh() {
            frames += 1;
            app.set_terminal(terminal.display());
        }
        let before = app.selected_key.clone();
        app.handle_key(KeyCode::Down, KeyModifiers::NONE);
        ensure!(app.selected_key != before, "navigation stalled");
        app.handle_key(KeyCode::Up, KeyModifiers::NONE);
        ensure!(app.selected_key == before, "navigation lost selection");
        ensure!(
            app.handle_key(KeyCode::Char('r'), KeyModifiers::NONE) == Effect::Refresh,
            "refresh was swallowed by terminal output"
        );
        let next = load_project_snapshot(&paths, Vec::new());
        ensure!(next.sources.len() == 1, "source refresh failed");
        app.refresh(next);
        ensure!(
            terminal.frame().cells.len() <= 60 * 10,
            "display exceeded viewport"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    ensure!(frames > 0, "no terminal output reached the dashboard");
    ensure!(
        start.elapsed() < Duration::from_secs(5),
        "display blocked navigation/refresh"
    );
    Ok(())
}

/// Render the production project view with multiple transport sources.
pub fn render_sources(sources: Vec<project::SourceSnapshot>) -> String {
    use ratatui::{Terminal, backend::TestBackend};
    let mut app = App::new(snapshot::DashboardSnapshot {
        rows: Vec::new(),
        errors: Vec::new(),
        sources,
        project_view: true,
    });
    app.resize(160, 35);
    let mut view = Terminal::new(TestBackend::new(160, 35)).unwrap();
    view.draw(|frame| render::draw(frame, &app)).unwrap();
    view.backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}
