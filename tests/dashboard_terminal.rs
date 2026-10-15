#![cfg(feature = "test-support")]

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use fluent::terminal_sessions::{TerminalIdentity, TerminalSession};
use fluent::terminal_view::{EmbeddedTerminal, Screen};
use std::time::{Duration, Instant};

fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
    Event::Key(KeyEvent::new(code, modifiers))
}

#[test]
fn escape_chord_preserves_native_keys_and_paste() {
    use fluent::terminal_view::Input;
    let mut screen = Screen::new(40, 8).unwrap();
    assert_eq!(
        screen.input(key(KeyCode::Esc, KeyModifiers::NONE)).unwrap(),
        Input::Bytes(vec![27])
    );
    assert_eq!(
        screen
            .input(key(KeyCode::Char('c'), KeyModifiers::CONTROL))
            .unwrap(),
        Input::Bytes(vec![3])
    );
    assert_eq!(
        screen
            .input(key(KeyCode::Char(']'), KeyModifiers::CONTROL))
            .unwrap(),
        Input::ReleaseFocus
    );
    assert_eq!(
        screen.input(key(KeyCode::Up, KeyModifiers::NONE)).unwrap(),
        Input::Bytes(b"\x1b[A".to_vec())
    );
    screen.write(b"\x1b[?1h\x1b[?2004h");
    assert_eq!(
        screen.input(key(KeyCode::Up, KeyModifiers::NONE)).unwrap(),
        Input::Bytes(b"\x1bOA".to_vec())
    );
    assert_eq!(
        screen.input(Event::Paste("a\n界".into())).unwrap(),
        Input::Bytes("\x1b[200~a\n界\x1b[201~".as_bytes().to_vec())
    );
    screen.write(b"\x1b[?2004l");
    assert_eq!(
        screen.input(Event::Paste("a\nb".into())).unwrap(),
        Input::Bytes(b"a\rb".to_vec())
    );
    screen.write(b"\x1b[>1u");
    assert_eq!(
        screen
            .input(key(KeyCode::Char('c'), KeyModifiers::CONTROL))
            .unwrap(),
        Input::Bytes(b"\x1b[99;5u".to_vec())
    );
    screen.write(b"\x1b[?1000h\x1b[?1006h");
    assert_eq!(
        screen
            .input(Event::Mouse(crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column: 2,
                row: 1,
                modifiers: KeyModifiers::NONE,
            }))
            .unwrap(),
        Input::Bytes(b"\x1b[<0;3;2M".to_vec())
    );
}

#[test]
fn terminal_input_revalidates_lease_and_generation() {
    use fluent::terminal_sessions::ControlOutcome;
    let root = tempfile::tempdir().unwrap();
    let identity = TerminalIdentity::fixture(root.path(), "lease", "attempt-1", "writer");
    let mut owner = TerminalSession::start_fixture(identity.clone(), "sleep 30").unwrap();
    let mut attachment = owner.observe(owner.descriptor()).unwrap();
    assert!(attachment.validate_input().is_err());
    assert_eq!(
        attachment.acquire_control().unwrap(),
        ControlOutcome::Acquired
    );
    attachment.validate_input().unwrap();
    owner.stop().unwrap();
    let mut replacement = TerminalSession::start_fixture(identity, "sleep 30").unwrap();
    assert!(attachment.validate_input().is_err());
    attachment.release_control();
    replacement.stop().unwrap();
}

fn fixture(root: &std::path::Path, name: &str) -> TerminalSession {
    TerminalSession::start_tmux_fixture(TerminalIdentity::fixture(
        root,
        name,
        "attempt-1",
        "writer",
    ))
    .unwrap()
}

fn wait_for(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !predicate() {
        assert!(Instant::now() < deadline, "terminal condition timed out");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn focused_input_reaches_only_selected_invocation() {
    let root = tempfile::tempdir().unwrap();
    let mut first = fixture(root.path(), "first");
    let mut second = fixture(root.path(), "second");
    let mut selected =
        EmbeddedTerminal::observe(first.observe(first.descriptor()).unwrap(), 60, 10).unwrap();
    let mut peer =
        EmbeddedTerminal::observe(second.observe(second.descriptor()).unwrap(), 60, 10).unwrap();
    assert!(
        selected
            .send(key(KeyCode::Char('x'), KeyModifiers::NONE))
            .is_err()
    );
    selected.acquire_control().unwrap();
    assert!(selected.display().state.contains("Control"));
    assert!(
        selected
            .display()
            .identity
            .contains(&first.descriptor().execution_generation)
    );
    let marker = root.path().join("selected");
    selected
        .send(Event::Paste(format!(
            "printf selected > '{}'",
            marker.display()
        )))
        .unwrap();
    selected
        .send(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    wait_for(|| marker.exists());
    assert!(peer.send(Event::Paste("forbidden".into())).is_err());
    assert!(
        !second
            .capture_pane_snapshot()
            .unwrap()
            .contains("printf selected")
    );
    let mut competitor = first.observe(first.descriptor()).unwrap();
    assert_eq!(
        competitor.acquire_control().unwrap(),
        fluent::terminal_sessions::ControlOutcome::Occupied
    );
    selected
        .send(key(KeyCode::Char(']'), KeyModifiers::CONTROL))
        .unwrap();
    assert!(!selected.focused());
    selected.send_literal_escape().unwrap();
    assert!(selected.focused());
    drop(selected);
    drop(peer);
    drop(competitor);
    first.stop().unwrap();
    second.stop().unwrap();
}

#[test]
fn observer_cannot_type_or_resize_worker() {
    let root = tempfile::tempdir().unwrap();
    let mut worker = fixture(root.path(), "observe");
    let size = || {
        let out = std::process::Command::new("tmux")
            .arg("-S")
            .arg(worker.socket_path().unwrap())
            .args([
                "display-message",
                "-p",
                "-t",
                "fluent:0.0",
                "#{pane_width}x#{pane_height}",
            ])
            .output()
            .unwrap();
        assert!(out.status.success());
        String::from_utf8(out.stdout).unwrap()
    };
    let before = size();
    let mut observer =
        EmbeddedTerminal::observe(worker.observe(worker.descriptor()).unwrap(), 30, 6).unwrap();
    assert!(
        observer
            .send(Event::Paste("touch forbidden\n".into()))
            .is_err()
    );
    observer.resize(45, 12).unwrap();
    wait_for(|| {
        observer.refresh();
        observer.frame().columns == 45
    });
    assert_eq!(size(), before);
    drop(observer);
    worker.stop().unwrap();
}

#[test]
fn worker_control_sequences_are_inert() {
    let mut screen = Screen::new(40, 8).unwrap();
    screen.write(b"\x1b[2J\x1b[Hsafe\x1b[38;2;201;33;55m red\x1b[0m\x1b]52;c;c2VjcmV0\x07\x1b]2;host-title\x07");
    let frame = screen.snapshot().unwrap();
    assert!(frame.text().contains("safe red"));
    assert!(!frame.text().contains("secret"));
    assert!(!frame.text().contains("host-title"));
    assert_eq!(frame.cells[5].foreground, [201, 33, 55]);
}

#[test]
fn worker_escape_sequences_stay_inside_terminal_view() {
    worker_control_sequences_are_inert();
    // The same projection consumes a real ordinary tmux client's repaint.
    let root = tempfile::tempdir().unwrap();
    let identity = TerminalIdentity::fixture(root.path(), "display", "attempt-1", "writer");
    let mut worker = TerminalSession::start_tmux_fixture(identity).unwrap();
    let status = std::process::Command::new("tmux")
        .arg("-S")
        .arg(worker.socket_path().unwrap())
        .args([
            "send-keys",
            "-l",
            "printf '\\033[2J\\033[Hembedded-%s\\n' proof",
        ])
        .status()
        .unwrap();
    assert!(status.success());
    assert!(
        std::process::Command::new("tmux")
            .arg("-S")
            .arg(worker.socket_path().unwrap())
            .args(["send-keys", "Enter"])
            .status()
            .unwrap()
            .success()
    );
    // Populate the pane before attaching so the client must repaint existing
    // contents. The echoed command deliberately does not contain the marker.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !worker
        .capture_pane_snapshot()
        .unwrap()
        .contains("embedded-proof")
    {
        assert!(
            Instant::now() < deadline,
            "fixture did not produce its marker"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let attachment = worker.observe(worker.descriptor()).unwrap();
    let mut terminal = EmbeddedTerminal::observe(attachment, 40, 8).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        terminal.refresh();
        if terminal.frame().text().contains("embedded-proof") {
            break;
        }
        assert!(Instant::now() < deadline, "{}", terminal.frame().text());
        std::thread::sleep(Duration::from_millis(20));
    }
    drop(terminal);
    worker.stop().unwrap();
}
