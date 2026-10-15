//! Drive the actual navigation and source refresh paths during live display IO.
use super::*;
use anyhow::ensure;
use crossterm::event::{KeyCode, KeyModifiers};

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
