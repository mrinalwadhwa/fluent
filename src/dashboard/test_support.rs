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

/// Enter Control after opening the terminal and returning to the narrow list.
pub fn control_from_narrow_list(
    root: &Path,
    terminal: &mut crate::terminal_view::EmbeddedTerminal,
) -> Result<()> {
    let mut app = App::new(load_project_snapshot(&[root.to_owned()], Vec::new()));
    app.resize(80, 24);
    ensure!(app.handle_key(KeyCode::Char('t'), KeyModifiers::NONE) == Effect::OpenTerminal);
    app.set_terminal(terminal.display());
    app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
    ensure!(app.pane == app::Pane::List);
    ensure!(app.handle_key(KeyCode::Char('i'), KeyModifiers::NONE) == Effect::ControlTerminal);
    terminal.acquire_control()?;
    app.set_terminal(terminal.display());
    assert_visible_control(&app)
}

/// Exercise dashboard navigation and the literal-chord action after focus returns.
pub fn navigate_then_send_literal_escape(
    root: &Path,
    terminal: &mut crate::terminal_view::EmbeddedTerminal,
    navigation: CEvent,
    literal: CEvent,
) -> Result<()> {
    ensure!(
        !terminal.focused(),
        "reserved chord did not restore navigation"
    );
    let mut app = App::new(load_project_snapshot(&[root.to_owned()], Vec::new()));
    app.resize(80, 24);
    app.handle_key(KeyCode::Char('t'), KeyModifiers::NONE);
    app.set_terminal(terminal.display());
    app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
    ensure!(app.pane == app::Pane::List);
    app.filter = project::Filter::Current;
    let keys = app.project_keys();
    ensure!(keys.len() >= 2, "fixture must publish two Work items");
    app.select_key(keys[0].clone());
    let before = app.selected_key.clone();
    let CEvent::Key(navigation) = navigation else {
        anyhow::bail!("expected a navigation key");
    };
    app.handle_key(navigation.code, navigation.modifiers);
    ensure!(
        app.selected_key != before,
        "dashboard navigation did not resume"
    );
    let CEvent::Key(literal) = literal else {
        anyhow::bail!("expected the literal-chord key");
    };
    app.set_terminal(terminal.display());
    ensure!(
        app.handle_key(literal.code, literal.modifiers) == Effect::LiteralEscape,
        "dashboard did not offer the literal chord"
    );
    terminal.send_literal_escape()?;
    app.set_terminal(terminal.display());
    assert_visible_control(&app)
}

/// Exercise the production resize and input gates while the worker captures bytes.
pub fn resize_control_and_resume(
    root: &Path,
    terminal: &mut crate::terminal_view::EmbeddedTerminal,
) -> Result<()> {
    let mut app = App::new(load_project_snapshot(&[root.to_owned()], Vec::new()));
    app.resize(80, 24);
    app.set_terminal(terminal.display());
    app.handle_key(KeyCode::Char('i'), KeyModifiers::NONE);
    ensure!(terminal.focused());
    for (width, height) in [(59, 24), (80, 14)] {
        resize_terminal(&mut app, Some(terminal), width, height);
        ensure!(!terminal.focused(), "resize left hidden Control focused");
        for event in [
            CEvent::Key(crossterm::event::KeyEvent::new(
                KeyCode::Char('x'),
                KeyModifiers::NONE,
            )),
            CEvent::Paste("hidden".into()),
        ] {
            ensure!(terminal_input_event(&app, event.clone()).is_none());
            ensure!(
                terminal.send(event).is_err(),
                "hidden input reached the worker"
            );
        }
        for key in ['i', ']'] {
            ensure!(app.handle_key(KeyCode::Char(key), KeyModifiers::NONE) == Effect::None);
        }
        resize_terminal(&mut app, Some(terminal), 80, 24);
        ensure!(!terminal.focused(), "resize silently restored input focus");
        ensure!(app.handle_key(KeyCode::Char(']'), KeyModifiers::NONE) == Effect::LiteralEscape);
        // This resume action contributes one literal Ctrl-] to the byte assertion.
        terminal.send_literal_escape()?;
        app.set_terminal(terminal.display());
        assert_visible_control(&app)?;
    }
    Ok(())
}

pub fn navigate_and_refresh(
    root: &Path,
    terminal: &mut crate::terminal_view::EmbeddedTerminal,
) -> Result<()> {
    let paths = vec![root.to_owned()];
    let mut app = App::new(load_project_snapshot(&paths, Vec::new()));
    app.filter = project::Filter::Current;
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
