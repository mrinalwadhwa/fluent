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
    let clients = || {
        let out = std::process::Command::new("tmux")
            .arg("-S")
            .arg(worker.socket_path().unwrap())
            .args([
                "list-clients",
                "-F",
                "#{client_width}x#{client_height}:#{client_readonly}",
            ])
            .output()
            .unwrap();
        assert!(out.status.success());
        String::from_utf8(out.stdout).unwrap()
    };
    // Wait for the server to see each viewport, not just the local parser.
    wait_for(|| clients().contains("30x6:1"));
    assert_eq!(size(), before, "the first observer resized the worker");
    assert!(
        observer
            .send(Event::Paste("touch forbidden\n".into()))
            .is_err()
    );
    observer.resize(45, 12).unwrap();
    wait_for(|| {
        observer.refresh();
        observer.frame().columns == 45 && clients().contains("45x12:1")
    });
    assert_eq!(size(), before);
    let mut peer =
        EmbeddedTerminal::observe(worker.observe(worker.descriptor()).unwrap(), 90, 30).unwrap();
    wait_for(|| clients().lines().count() == 2);
    assert_eq!(size(), before, "multiple observers resized the worker");
    observer.reconnect().unwrap();
    wait_for(|| clients().contains("45x12:1"));
    assert_eq!(
        size(),
        before,
        "reconnecting an observer resized the worker"
    );

    peer.acquire_control().unwrap();
    peer.resize(73, 13).unwrap();
    wait_for(|| clients().contains("73x13:0"));
    assert_eq!(size().trim(), "73x13");
    peer.observe_only().unwrap();
    wait_for(|| clients().lines().count() == 2 && !clients().contains(":0"));
    observer.resize(35, 9).unwrap();
    wait_for(|| clients().contains("35x9:1"));
    assert_eq!(
        size().trim(),
        "73x13",
        "releasing Control lost the worker size"
    );
    drop(peer);
    drop(observer);
    worker.stop().unwrap();
}

#[test]
fn screen_resize_preserves_terminal_screen_semantics() {
    let mut screen = Screen::new(20, 6).unwrap();
    screen.write("primary 界e\u{301}".as_bytes());
    screen.write(b"\x1b[?1049h\x1b[2J\x1b[H");
    screen.write("alternate 界".as_bytes());
    screen.write(b"\x1b[3;4H\x1b[?25h");
    screen.resize(30, 10).unwrap();
    let frame = screen.snapshot().unwrap();
    assert_eq!((frame.columns, frame.rows), (30, 10));
    assert_eq!(frame.cursor, Some((3, 2)));
    assert!(frame.text().contains("alternate 界"));
    assert!(!frame.text().contains("primary"));
    assert_eq!(frame.cells[10].width, 2);
    assert_eq!(frame.cells[11].width, 0);
    screen.write(b"\x1b[?1049l");
    assert!(
        screen
            .snapshot()
            .unwrap()
            .text()
            .contains("primary 界e\u{301}")
    );
    assert!(screen.resize(501, 10).is_err());
}

#[test]
fn resize_preserves_terminal_screen_semantics() {
    screen_resize_preserves_terminal_screen_semantics();
    let root = tempfile::tempdir().unwrap();
    let mut worker = fixture(root.path(), "resize");
    let mut view =
        EmbeddedTerminal::observe(worker.observe(worker.descriptor()).unwrap(), 60, 10).unwrap();
    view.acquire_control().unwrap();
    view.resize(73, 13).unwrap();
    wait_for(|| {
        view.refresh();
        (view.frame().columns, view.frame().rows) == (73, 13)
    });
    let size = std::process::Command::new("tmux")
        .arg("-S")
        .arg(worker.socket_path().unwrap())
        .args(["display-message", "-p", "#{pane_width}x#{pane_height}"])
        .output()
        .unwrap();
    assert!(size.status.success());
    assert_eq!(String::from_utf8(size.stdout).unwrap().trim(), "73x13");
    drop(view);
    worker.stop().unwrap();
}

#[test]
fn ended_view_does_not_follow_new_invocation_input() {
    let root = tempfile::tempdir().unwrap();
    let mut worker = fixture(root.path(), "end");
    let mut view =
        EmbeddedTerminal::observe(worker.observe(worker.descriptor()).unwrap(), 60, 10).unwrap();
    view.acquire_control().unwrap();
    let generation = view.descriptor().execution_generation.clone();
    // End the real server, retain the frontend, and publish a replacement.
    std::thread::scope(|scope| {
        let stopped = scope.spawn(|| worker.stop());
        wait_for(|| {
            view.refresh();
            view.ended()
        });
        stopped.join().unwrap().unwrap();
    });
    let final_frame = view.frame().text();
    let mut replacement = fixture(root.path(), "end");
    assert_ne!(replacement.descriptor().execution_generation, generation);
    assert_eq!(view.descriptor().execution_generation, generation);
    assert!(view.display().state.contains("Ended"));
    assert!(view.send(Event::Paste("forbidden".into())).is_err());
    assert!(view.acquire_control().is_err());
    view.refresh();
    assert_eq!(view.frame().text(), final_frame);
    drop(view);
    replacement.stop().unwrap();
}

#[test]
fn fullscreen_return_restores_dashboard_after_exit_or_failure() {
    let output = std::process::Command::new("python3")
        .arg("tests/fixtures/dashboard-terminal-handoff.py")
        .arg(std::env::current_exe().unwrap())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn fullscreen_resize_requires_control() {
    for mode in ["observe", "control"] {
        let output = std::process::Command::new("python3")
            .arg("tests/fixtures/dashboard-terminal-handoff.py")
            .arg(std::env::current_exe().unwrap())
            .arg(mode)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{mode}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn fullscreen_attachment_pty_child() {
    let Ok(mode) = std::env::var("FLUENT_ATTACHMENT_PTY_MODE") else {
        return;
    };
    use std::io::Write;
    let root = tempfile::tempdir().unwrap();
    let mut worker = fixture(root.path(), "fullscreen-resize");
    let mut attachment = worker.observe(worker.descriptor()).unwrap();
    if mode == "control" {
        assert_eq!(
            attachment.acquire_control().unwrap(),
            fluent::terminal_sessions::ControlOutcome::Acquired
        );
    }
    let socket = worker.socket_path().unwrap().to_owned();
    let driver = std::thread::spawn(move || {
        let query = |args: &[&str]| {
            let output = std::process::Command::new("tmux")
                .env_remove("TMUX")
                .arg("-S")
                .arg(&socket)
                .args(args)
                .output()
                .unwrap();
            assert!(output.status.success());
            String::from_utf8(output.stdout).unwrap()
        };
        let result = std::panic::catch_unwind(|| {
            let size = || query(&["display-message", "-p", "#{pane_width}x#{pane_height}"]);
            let clients = || query(&["list-clients", "-F", "#{client_width}x#{client_height}"]);
            wait_for(|| clients().trim() == "100x30");
            let initial = if mode == "control" { "100x30" } else { "80x24" };
            wait_for(|| size().trim() == initial);
            println!("RESIZE_ATTACH_READY");
            std::io::stdout().flush().unwrap();
            wait_for(|| clients().trim() == "73x13");
            let expected = if mode == "control" { "73x13" } else { "80x24" };
            wait_for(|| size().trim() == expected);
        });
        // Always release the blocking native attachment, even after an assertion.
        query(&["detach-client", "-s", "fluent"]);
        result.unwrap();
    });
    let result = attachment.run();
    let driven = driver.join();
    worker.stop().unwrap();
    assert_eq!(result.unwrap(), 0);
    driven.unwrap();
}

#[test]
fn fullscreen_pty_child() {
    if std::env::var_os("FLUENT_HANDOFF_PTY_CHILD").is_none() {
        return;
    }
    use fluent::dashboard::terminal::DashboardTerminal;

    // Starting a managed server from this terminal must leave stdin available
    // for the native attachment below. A tmux server that inherits this PTY
    // rejects a later client attaching from the same terminal.
    assert_eq!(unsafe { libc::isatty(libc::STDIN_FILENO) }, 1);
    let root = tempfile::tempdir().unwrap();
    let marker = root.path().join("server-stdin");
    let mut server = TerminalSession::start_fixture(
        TerminalIdentity::fixture(root.path(), "server-stdin", "attempt-1", "writer"),
        &format!(
            "if test /dev/fd/0 -ef /dev/null; then printf detached; else printf inherited; fi > '{}'; exec sleep 30",
            marker.display()
        ),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !std::fs::read_to_string(&marker).is_ok_and(|value| !value.is_empty())
        && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(10));
    }
    server.stop().unwrap();
    assert_eq!(std::fs::read_to_string(marker).unwrap(), "detached");

    let mut terminal = DashboardTerminal::open().unwrap();
    terminal.set_mouse_enabled(false).unwrap();
    for outcome in [0, 7, -1] {
        assert!(crossterm::terminal::is_raw_mode_enabled().unwrap());
        let result = terminal.handoff(|| -> anyhow::Result<i32> {
            assert!(!crossterm::terminal::is_raw_mode_enabled().unwrap());
            // Exercise inherited-terminal process exit and attachment failure.
            if outcome < 0 {
                return Err(anyhow::anyhow!("attachment failed"));
            }
            let status = std::process::Command::new("/bin/sh")
                .args(["-c", &format!("test -t 0 && exit {outcome}")])
                .status()?;
            Ok(status.code().unwrap())
        });
        if outcome < 0 {
            assert!(result.is_err());
        } else {
            assert_eq!(result.unwrap(), outcome);
        }
        assert!(crossterm::terminal::is_raw_mode_enabled().unwrap());
        assert!(!terminal.mouse_enabled());
        assert_eq!(terminal.size().unwrap(), (100, 30));
    }
    drop(terminal);
    assert!(!crossterm::terminal::is_raw_mode_enabled().unwrap());
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

#[test]
fn display_flood_keeps_dashboard_responsive() {
    let root = tempfile::tempdir().unwrap();
    let store = fluent::work_model::WorkModelStore::new(root.path());
    for id in ["first", "second"] {
        store
            .create_work_item(&fluent::work_model::WorkItem::planned(id, id))
            .unwrap();
    }
    let mut worker = fixture(root.path(), "flood");
    let mut view =
        EmbeddedTerminal::observe(worker.observe(worker.descriptor()).unwrap(), 60, 10).unwrap();
    let socket = worker.socket_path().unwrap();
    let command = r"i=0; while [ $i -lt 20000 ]; do printf '\033[Hflood-%s 界\n' $i; i=$((i+1)); done; printf 'FLOOD-DONE\n'";
    assert!(
        std::process::Command::new("tmux")
            .arg("-S")
            .arg(socket)
            .args(["send-keys", "-l", command])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        std::process::Command::new("tmux")
            .arg("-S")
            .arg(socket)
            .args(["send-keys", "Enter"])
            .status()
            .unwrap()
            .success()
    );
    fluent::dashboard::test_support::navigate_and_refresh(root.path(), &mut view).unwrap();
    // Reconnect discards parser state, asks tmux for a complete repaint, and
    // does not silently substitute a later generation.
    let descriptor = view.descriptor().clone();
    view.reconnect().unwrap();
    assert_eq!(view.descriptor(), &descriptor);
    wait_for(|| {
        view.refresh();
        view.frame().text().contains("FLOOD-DONE")
    });
    assert!(view.frame().cells.len() <= 60 * 10);
    drop(view);
    worker.stop().unwrap();
}

#[test]
fn display_projection_bounds_frames_and_reports_lost_sync() {
    let mut screen = Screen::new(80, 24).unwrap();
    let chunk = "\x1b[Hflood 界e\u{301}\r\n".repeat(1024);
    for _ in 0..128 {
        screen.write(chunk.as_bytes());
        let frame = screen.snapshot().unwrap();
        assert_eq!(frame.cells.len(), 80 * 24);
        assert!(frame.cells.iter().all(|cell| cell.text().len() <= 128));
    }
    // A burst of terminal queries exceeds the bounded reply mailbox.
    screen.write(b"\x1b[6n".repeat(2000).as_slice());
    assert!(screen.snapshot().is_err());
    let mut reconnected = Screen::new(80, 24).unwrap();
    reconnected.write(b"\x1b[2J\x1b[Hrepainted");
    assert!(reconnected.snapshot().unwrap().text().contains("repainted"));
}
