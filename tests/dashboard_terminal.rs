#![cfg(feature = "test-support")]

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use fluent::terminal_sessions::{TerminalIdentity, TerminalSession};
use fluent::terminal_view::{EmbeddedTerminal, Screen};
use std::time::{Duration, Instant};

fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
    Event::Key(KeyEvent::new(code, modifiers))
}

#[test]
fn canonical_dashboard_host_gate_keeps_real_tmux_required() {
    let ordinary: serde_yaml::Value =
        serde_yaml::from_str(include_str!("../.fluent/tester.yaml")).unwrap();
    assert!(
        ordinary["commands"]
            .as_sequence()
            .unwrap()
            .iter()
            .all(|command| !command["command"]
                .as_str()
                .unwrap()
                .contains("tests/host/check-")),
        "ordinary Tester must leave host checks to release acceptance"
    );
    let config: serde_yaml::Value =
        serde_yaml::from_str(include_str!("../.fluent/release-host.yaml")).unwrap();
    let commands = config["commands"].as_sequence().unwrap();
    let command = commands
        .iter()
        .find(|command| {
            command["command"].as_str() == Some("bash tests/host/check-dashboard-terminal.sh")
        })
        .expect("real tmux coverage must remain a required release host command");
    assert_eq!(command["test_harness"].as_str(), Some("cargo-nextest"));
    assert_eq!(command["reject_process_leaks"].as_bool(), Some(true));
    let output = std::process::Command::new("bash")
        .arg("tests/host/check-dashboard-terminal.sh")
        .env_remove("FLUENT_TEST_REAL_TERMINAL_HOST")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("host terminal authorization is required")
    );
}

fn confined_child(test: &str, flood: bool) -> bool {
    if std::env::var("FLUENT_TEST_DASHBOARD_CHILD").as_deref() == Ok(test) {
        return true;
    }
    let logs = tempfile::tempdir().unwrap();
    let mut client = std::env::current_dir()
        .unwrap()
        .join("tests/fixtures/dashboard-terminal-client.py");
    if flood {
        use std::os::unix::fs::PermissionsExt;

        // Exceed the navigation probe's duration to exercise independent startup.
        let delayed = logs.path().join("delayed-client.py");
        std::fs::write(
            &delayed,
            format!(
                "#!/usr/bin/env python3\nimport os, sys, time\ntime.sleep(0.6)\nos.execv(sys.executable, [sys.executable, {}, *sys.argv[1:]])\n",
                serde_json::to_string(&client).unwrap()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&delayed, std::fs::Permissions::from_mode(0o700)).unwrap();
        client = delayed;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env("FLUENT_TEST_DASHBOARD_CHILD", test)
        .env("FLUENT_TEST_DASHBOARD_FLOOD", if flood { "1" } else { "0" })
        .env("FLUENT_TEST_TERMINAL_CLIENT", client)
        .env("FLUENT_TEST_TERMINAL_CLIENT_LOG", logs.path().join("input"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "child {}: {}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    false
}

#[test]
fn confined_terminal_enforces_control_and_generation() {
    if !confined_child("confined_terminal_enforces_control_and_generation", false) {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let identity = TerminalIdentity::fixture(root.path(), "confined", "attempt-1", "writer");
    let mut worker = TerminalSession::start_fixture(identity.clone(), "exec sleep 30").unwrap();
    let mut view =
        EmbeddedTerminal::observe(worker.observe(worker.descriptor()).unwrap(), 60, 10).unwrap();
    wait_for(|| {
        view.refresh();
        view.frame().text().contains("CLIENT_READY observe")
    });
    assert!(view.send(Event::Paste("forbidden".into())).is_err());
    view.resize(45, 12).unwrap();
    wait_for(|| {
        view.refresh();
        (view.frame().columns, view.frame().rows) == (45, 12)
    });
    view.acquire_control().unwrap();
    wait_for(|| {
        view.refresh();
        view.frame().text().contains("CLIENT_READY control")
    });
    let mut peer = worker.observe(worker.descriptor()).unwrap();
    assert_eq!(
        peer.acquire_control().unwrap(),
        fluent::terminal_sessions::ControlOutcome::Occupied
    );
    view.send(Event::Paste("selected-界".into())).unwrap();
    let input = std::env::var_os("FLUENT_TEST_TERMINAL_CLIENT_LOG").unwrap();
    wait_for(|| std::fs::read(&input).is_ok_and(|bytes| bytes == "selected-界".as_bytes()));
    view.observe_only().unwrap();
    assert_eq!(
        peer.acquire_control().unwrap(),
        fluent::terminal_sessions::ControlOutcome::Acquired
    );
    peer.release_control();
    view.acquire_control().unwrap();
    let descriptor = view.descriptor().clone();
    std::thread::scope(|scope| {
        let stopped = scope.spawn(|| worker.stop());
        wait_for(|| {
            view.refresh();
            view.ended()
        });
        stopped.join().unwrap().unwrap();
    });
    let frame = view.frame().text();
    let mut replacement = TerminalSession::start_fixture(identity, "exec sleep 30").unwrap();
    assert_ne!(replacement.descriptor(), &descriptor);
    assert_eq!(view.descriptor(), &descriptor);
    assert!(view.send(Event::Paste("forbidden".into())).is_err());
    assert!(view.acquire_control().is_err());
    view.refresh();
    assert_eq!(view.frame().text(), frame);
    drop(view);
    replacement.stop().unwrap();
}

#[test]
fn confined_resize_requires_live_control() {
    if !confined_child("confined_resize_requires_live_control", false) {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let identity = TerminalIdentity::fixture(root.path(), "resize-control", "attempt-1", "writer");
    let mut worker = TerminalSession::start_fixture(identity, "exec sleep 30").unwrap();
    let mut attachment = worker.observe(worker.descriptor()).unwrap();

    assert!(
        attachment
            .resize(73, 13)
            .unwrap_err()
            .to_string()
            .contains("Control is required")
    );
    assert_eq!(
        attachment.acquire_control().unwrap(),
        fluent::terminal_sessions::ControlOutcome::Acquired
    );
    attachment.resize(73, 13).unwrap();
    attachment.release_control();
    assert!(attachment.resize(73, 13).is_err());

    worker.stop().unwrap();
    assert!(attachment.resize(73, 13).is_err());
}

#[test]
fn confined_terminal_flood_preserves_dashboard_navigation() {
    if !confined_child(
        "confined_terminal_flood_preserves_dashboard_navigation",
        true,
    ) {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let store = fluent::work_model::WorkModelStore::new(root.path());
    for id in ["first", "second"] {
        store
            .create_work_item(&fluent::work_model::WorkItem::planned(id, id))
            .unwrap();
    }
    let mut worker = TerminalSession::start_fixture(
        TerminalIdentity::fixture(root.path(), "confined-flood", "attempt-1", "writer"),
        "exec sleep 30",
    )
    .unwrap();
    let mut view =
        EmbeddedTerminal::observe(worker.observe(worker.descriptor()).unwrap(), 60, 10).unwrap();
    wait_for(|| {
        view.refresh();
        view.frame().text().contains("flood 界")
    });
    fluent::dashboard::test_support::navigate_and_refresh(root.path(), &mut view).unwrap();
    let descriptor = view.descriptor().clone();
    view.reconnect().unwrap();
    assert_eq!(view.descriptor(), &descriptor);
    wait_for(|| {
        view.refresh();
        view.frame().text().contains("flood 界")
    });
    assert!(view.frame().cells.len() <= 60 * 10);
    drop(view);
    worker.stop().unwrap();
}

fn run_input_pty(mode: &str) {
    let output = std::process::Command::new("python3")
        .arg("tests/fixtures/dashboard-terminal-handoff.py")
        .arg(std::env::current_exe().unwrap())
        .arg(mode)
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
fn pty_handoff_timeout_retains_diagnostics() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().unwrap();
    let child = root.path().join("stalled-terminal.py");
    std::fs::write(
        &child,
        "#!/usr/bin/env python3\nimport os, signal\nos.write(2, b'original attachment failure\\n')\nsignal.pause()\n",
    )
    .unwrap();
    std::fs::set_permissions(&child, std::fs::Permissions::from_mode(0o700)).unwrap();
    let output = std::process::Command::new("python3")
        .arg("tests/fixtures/dashboard-terminal-handoff.py")
        .arg(child)
        .output()
        .unwrap();
    assert!(!output.status.success(), "a stalled child must fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("terminal handoff child did not settle"));
    assert!(
        stderr.contains("original attachment failure"),
        "timeout discarded the original terminal diagnostic: {stderr}"
    );
}

fn read_pty_input(step: &str) -> Event {
    use std::io::Write;
    println!("INPUT_READY_{step}");
    std::io::stdout().flush().unwrap();
    assert!(crossterm::event::poll(Duration::from_secs(5)).unwrap());
    crossterm::event::read().unwrap()
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
            .input(key(KeyCode::Char('b'), KeyModifiers::CONTROL))
            .unwrap(),
        Input::Bytes(vec![2])
    );
    assert_eq!(
        screen
            .input(key(KeyCode::Char(']'), KeyModifiers::CONTROL))
            .unwrap(),
        Input::ReleaseFocus
    );
    for code in [KeyCode::Char(']'), KeyCode::Char('5')] {
        for modifiers in [
            KeyModifiers::NONE,
            KeyModifiers::ALT,
            KeyModifiers::CONTROL | KeyModifiers::ALT,
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ] {
            assert!(!fluent::terminal_view::releases_focus(&key(
                code, modifiers
            )));
        }
        assert!(!fluent::terminal_view::releases_focus(&Event::Key(
            KeyEvent::new_with_kind(
                code,
                KeyModifiers::CONTROL,
                crossterm::event::KeyEventKind::Release,
            )
        )));
    }
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
    run_input_pty("input");
}

#[test]
fn terminal_input_pty_child() {
    if std::env::var_os("FLUENT_INPUT_PTY_CHILD").is_none() {
        return;
    }
    use fluent::terminal_view::Input;
    let _terminal = fluent::dashboard::terminal::DashboardTerminal::open().unwrap();
    let mut screen = Screen::new(40, 8).unwrap();
    screen.write(b"\x1b[?2004h");
    for (step, expected) in [
        ("esc", b"\x1b".to_vec()),
        ("interrupt", b"\x03".to_vec()),
        ("prefix", b"\x02".to_vec()),
        ("after_prefix", b"d".to_vec()),
        ("paste", "\x1b[200~native-界\x1b[201~".as_bytes().to_vec()),
    ] {
        assert_eq!(
            screen.input(read_pty_input(step)).unwrap(),
            Input::Bytes(expected)
        );
    }
    for step in ["chord", "enhanced_chord"] {
        let event = read_pty_input(step);
        assert_eq!(
            screen.input(event.clone()).unwrap(),
            Input::ReleaseFocus,
            "reserved chord decoded as {event:?}"
        );
    }
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
    run_input_pty("control-input");
}

#[test]
fn terminal_control_input_pty_child() {
    if std::env::var_os("FLUENT_CONTROL_INPUT_PTY_CHILD").is_none() {
        return;
    }
    let _terminal = fluent::dashboard::terminal::DashboardTerminal::open().unwrap();
    let root = tempfile::tempdir().unwrap();
    let store = fluent::work_model::WorkModelStore::new(root.path());
    let mut item = fluent::work_model::WorkItem::planned("input", "Two selectable workers");
    item.add_initial_attempt("attempt-1").unwrap();
    let mut next = item.attempts[0].tasks[0].clone();
    next.id = "attempt-1-write-2".into();
    next.artifact_area.as_mut().unwrap().path =
        fluent::work_model::work_artifact_path("input", "attempt-1", &next.id);
    item.attempts[0].tasks.push(next);
    store.create_work_item(&item).unwrap();
    let worker = |task| {
        TerminalSession::start_tmux_fixture(TerminalIdentity::fixture(
            root.path(),
            "input",
            "attempt-1",
            task,
        ))
        .unwrap()
    };
    let mut first = worker("attempt-1-write-1");
    let mut second = worker("attempt-1-write-2");
    let mut dashboard =
        fluent::dashboard::test_support::TerminalDashboard::open(root.path()).unwrap();
    assert_eq!(dashboard.terminal().descriptor(), first.descriptor());
    let mut peer =
        EmbeddedTerminal::observe(second.observe(second.descriptor()).unwrap(), 60, 10).unwrap();
    assert!(
        dashboard
            .terminal()
            .send(key(KeyCode::Char('x'), KeyModifiers::NONE))
            .is_err()
    );
    dashboard.control_from_narrow_list().unwrap();
    let selected = dashboard.terminal();
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
    // Capture exact worker bytes so Esc, Ctrl-C and tmux's Ctrl-B stay native.
    // Deliver the literal chord while this same invocation remains selected.
    let captured = root.path().join("native-input");
    let capture_done = root.path().join("native-input-done");
    let expected = "\x1b\x03\x02dnative-界\x1d\x1d\x1dZ".as_bytes();
    selected
        .send(Event::Paste(format!(
            "stty raw -echo; dd bs=1 count={} of='{}' 2>/dev/null; stty sane; touch '{}'",
            expected.len(),
            captured.display(),
            capture_done.display()
        )))
        .unwrap();
    selected
        .send(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    wait_for(|| captured.exists());
    for step in ["esc", "interrupt", "prefix", "after_prefix", "paste"] {
        selected.send(read_pty_input(step)).unwrap();
    }
    selected.send(read_pty_input("chord")).unwrap();
    assert!(!selected.focused());
    let literal = read_pty_input("literal");
    dashboard.key(literal).unwrap();
    let selected = dashboard.terminal();
    assert!(selected.focused());
    dashboard.resize_control_and_resume().unwrap();
    let selected = dashboard.terminal();
    selected.send(read_pty_input("sentinel")).unwrap();
    wait_for(|| std::fs::metadata(&captured).unwrap().len() >= expected.len() as u64);
    assert_eq!(std::fs::read(&captured).unwrap(), expected);
    wait_for(|| capture_done.exists());

    // Keep the first worker capturing until a sentinel arrives after the second
    // worker finishes. Any leaked navigation or second-worker input is visible.
    let first_after_navigation = root.path().join("first-after-navigation");
    selected
        .send(Event::Paste(format!(
            "stty raw -echo; dd bs=1 count=1 of='{}' 2>/dev/null; stty sane",
            first_after_navigation.display()
        )))
        .unwrap();
    selected
        .send(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    wait_for(|| first_after_navigation.exists());
    selected.send(read_pty_input("navigate_chord")).unwrap();
    assert!(!selected.focused());
    dashboard
        .key(key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    dashboard.key(read_pty_input("navigation")).unwrap();
    dashboard
        .key(key(KeyCode::Right, KeyModifiers::NONE))
        .unwrap();
    let selected = dashboard.terminal();
    assert_eq!(selected.descriptor(), second.descriptor());
    assert_eq!(selected.state_label(), "Observe · i control");
    assert!(!selected.has_control());
    assert!(!selected.focused());
    assert!(selected.send(Event::Paste("forbidden".into())).is_err());
    assert!(
        selected
            .send(key(KeyCode::Char('x'), KeyModifiers::NONE))
            .is_err()
    );
    assert_eq!(
        competitor.acquire_control().unwrap(),
        fluent::terminal_sessions::ControlOutcome::Acquired
    );
    competitor.release_control();
    // The literal action must not silently acquire the new invocation's lease.
    dashboard
        .key(key(KeyCode::Char(']'), KeyModifiers::NONE))
        .unwrap();
    assert!(!dashboard.terminal().has_control());
    assert!(!dashboard.terminal().focused());
    dashboard.key(read_pty_input("control_second")).unwrap();
    dashboard.assert_visible_control().unwrap();
    let selected = dashboard.terminal();
    assert_eq!(selected.descriptor(), second.descriptor());
    assert!(selected.has_control());
    let second_capture = root.path().join("second-input");
    let second_expected = b"secondZ";
    selected
        .send(Event::Paste(format!(
            "stty raw -echo; dd bs=1 count={} of='{}' 2>/dev/null; stty sane",
            second_expected.len(),
            second_capture.display()
        )))
        .unwrap();
    selected
        .send(key(KeyCode::Enter, KeyModifiers::NONE))
        .unwrap();
    wait_for(|| second_capture.exists());
    selected.send(read_pty_input("second_input")).unwrap();
    wait_for(|| std::fs::metadata(&second_capture).unwrap().len() >= second_expected.len() as u64);
    assert_eq!(std::fs::read(second_capture).unwrap(), second_expected);
    assert!(std::fs::read(&first_after_navigation).unwrap().is_empty());
    let mut first_again =
        EmbeddedTerminal::observe(first.observe(first.descriptor()).unwrap(), 60, 10).unwrap();
    first_again.acquire_control().unwrap();
    first_again
        .send(key(KeyCode::Char('A'), KeyModifiers::NONE))
        .unwrap();
    wait_for(|| std::fs::metadata(&first_after_navigation).unwrap().len() >= 1);
    assert_eq!(std::fs::read(first_after_navigation).unwrap(), b"A");
    assert_eq!(std::fs::read(captured).unwrap(), expected);
    drop(first_again);
    drop(dashboard);
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
fn fullscreen_handoff_keeps_dashboard_responsive_after_partial_input() {
    for input in [
        "control",
        "partial",
        "terminal-report",
        "fragmented-report",
        "separated-report",
        "literal-paste",
        "unfinished",
    ] {
        for mode in ["observe", "control"] {
            for outcome in ["detach", "failure"] {
                let output = std::process::Command::new("python3")
                    .arg("tests/fixtures/dashboard-partial-handoff.py")
                    .arg(std::env::current_exe().unwrap())
                    .arg(mode)
                    .arg(outcome)
                    .arg(input)
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "{mode} {outcome} input={input}: {}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }
    }
}

#[test]
fn fullscreen_handoff_bounds_detach_after_slow_client_startup() {
    let output = std::process::Command::new("python3")
        .arg("tests/fixtures/dashboard-partial-handoff.py")
        .arg(std::env::current_exe().unwrap())
        .args(["control", "detach", "control", "slow-start"])
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
fn fullscreen_handoff_accepts_detach_during_output_pressure() {
    for mode in ["observe", "control"] {
        let output = std::process::Command::new("python3")
            .arg("tests/fixtures/dashboard-partial-handoff.py")
            .arg(std::env::current_exe().unwrap())
            .args([mode, "detach", "control", "pressure"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{mode}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("detached before display drain"));
    }
}

#[test]
fn partial_handoff_pty_child() {
    let Ok(mode) = std::env::var("FLUENT_PARTIAL_HANDOFF_MODE") else {
        return;
    };
    use fluent::dashboard::terminal::DashboardTerminal;
    use std::io::Write;
    let root = tempfile::tempdir().unwrap();
    let mut owner = TerminalSession::start_fixture(
        TerminalIdentity::fixture(root.path(), "partial", "attempt-1", "writer"),
        &format!(
            "echo $$ > '{}'; exec sleep 15",
            std::env::var("FLUENT_PARTIAL_HANDOFF_WORKER_PID").unwrap()
        ),
    )
    .unwrap();
    let mut view =
        EmbeddedTerminal::observe(owner.observe(owner.descriptor()).unwrap(), 60, 10).unwrap();
    if mode == "control" {
        view.acquire_control().unwrap();
    }
    let descriptor = view.descriptor().clone();
    let mut terminal = DashboardTerminal::open().unwrap();
    let flags = [unsafe { libc::fcntl(0, libc::F_GETFL) }, unsafe {
        libc::fcntl(1, libc::F_GETFL)
    }];
    println!("PARTIAL_DASHBOARD_READY");
    std::io::stdout().flush().unwrap();
    assert!(crossterm::event::poll(Duration::from_secs(1)).unwrap());
    assert_eq!(
        crossterm::event::read().unwrap(),
        key(KeyCode::Char('f'), KeyModifiers::NONE)
    );
    let result = terminal.handoff(|| view.fullscreen(100, 30));
    if std::env::var("FLUENT_PARTIAL_HANDOFF_OUTCOME").as_deref() == Ok("failure") {
        assert_eq!(result.unwrap(), 7);
    } else {
        assert_eq!(result.unwrap(), 0);
    }
    assert_eq!(view.descriptor(), &descriptor);
    assert_eq!(view.has_control(), mode == "control");
    assert!(owner.is_live().unwrap());
    assert!(crossterm::terminal::is_raw_mode_enabled().unwrap());
    assert_eq!(
        flags,
        [unsafe { libc::fcntl(0, libc::F_GETFL) }, unsafe {
            libc::fcntl(1, libc::F_GETFL)
        }]
    );
    println!("PARTIAL_DASHBOARD_RETURNED");
    std::io::stdout().flush().unwrap();
    fluent::dashboard::test_support::assert_handoff_navigation().unwrap();
    drop(view);
    owner.stop().unwrap();
    drop(terminal);
    assert!(!crossterm::terminal::is_raw_mode_enabled().unwrap());
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
    let mut failures = Vec::new();
    for mode in ["observe", "control"] {
        let output = std::process::Command::new("python3")
            .arg("tests/fixtures/dashboard-terminal-handoff.py")
            .arg(std::env::current_exe().unwrap())
            .arg(mode)
            .output()
            .unwrap();
        if !output.status.success() {
            failures.push(format!(
                "{mode}: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
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
            assert!(
                output.status.success(),
                "tmux {args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).unwrap()
        };
        let result = std::panic::catch_unwind(|| {
            let size = || query(&["display-message", "-p", "#{pane_width}x#{pane_height}"]);
            let clients = || query(&["list-clients", "-F", "#{client_width}x#{client_height}"]);
            wait_for(|| clients().trim() == "100x30");
            let initial = if mode == "control" { "100x30" } else { "80x24" };
            wait_for(|| size().trim() == initial);
            query(&[
                "set-hook",
                "-g",
                "client-detached",
                "set-option -g @fluent_fixture_detached 1",
            ]);
            println!("RESIZE_ATTACH_READY");
            std::io::stdout().flush().unwrap();
            wait_for(|| clients().trim() == "73x13");
            let expected = if mode == "control" { "73x13" } else { "80x24" };
            wait_for(|| size().trim() == expected);
            println!("DETACH_ATTACH_READY");
            std::io::stdout().flush().unwrap();
            wait_for(|| clients().trim().is_empty());
            wait_for(|| {
                query(&["display-message", "-p", "#{@fluent_fixture_detached}"]).trim() == "1"
            });
            query(&["has-session", "-t", "fluent"]);
            assert_eq!(size().trim(), expected, "detach changed the worker size");
        });
        // Release a blocking attachment on failure; success must use Ctrl-B d.
        if result.is_err() {
            query(&["detach-client", "-s", "fluent"]);
        }
        result.unwrap();
    });
    let result = attachment.run();
    eprintln!("full-screen attachment returned: {result:?}");
    let driven = driver.join();
    worker.stop().unwrap();
    driven.expect("the server must confirm resize and native detach");
    // tmux 3.5a can return 1 after Ctrl-B d. Accept that outcome only after
    // the driver confirms client-detached, a live session, and its final size.
    let status = result.unwrap();
    assert!(
        matches!(status, 0 | 1),
        "unexpected tmux exit status {status}"
    );
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

const DASHBOARD_FLOOD_PRODUCER: &str = r#"deadline=$(($(date +%s) + ${4:-15}))
alive() { [ -d "$3" ] && [ "$(date +%s)" -lt "$deadline" ]; }
while [ ! -f "$1" ]; do
    alive "$@" || exit 1
    sleep 0.01
done
i=0
while [ "$i" -lt 20000 ] || [ ! -f "$2" ]; do
    if [ $((i%1000)) -eq 0 ]; then
        alive "$@" || exit 1
        sleep 0.01
    fi
    printf '\033[Hflood-%s 界\n' "$i"
    i=$((i+1))
done
printf 'FLOOD-DONE\n'
exec sleep 30
"#;

#[test]
fn flood_producer_bounds_failed_gates() {
    // Kill and reap even when an assertion fails; this regression must never
    // leave the very producer it checks running after the test exits.
    struct Producer(std::process::Child);
    impl Drop for Producer {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    use std::os::unix::fs::PermissionsExt;

    for scenario in ["no-start", "no-finish", "removed-directory", "normal"] {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("fixture");
        std::fs::create_dir(&directory).unwrap();
        let start = directory.join("start");
        let finish = directory.join("finish");
        let output = root.path().join("output");
        let errors = root.path().join("errors");
        let output_text = || String::from_utf8_lossy(&std::fs::read(&output).unwrap()).into_owned();

        // Override only this child's clock. Keep the clock outside the fixture
        // directory so removing that directory exercises the producer's guard.
        let clock = root.path().join("clock");
        let clock_reads = root.path().join("clock-reads");
        std::fs::write(&clock, b"100\n").unwrap();
        std::fs::write(&clock_reads, b"").unwrap();
        let bin = root.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let date = bin.join("date");
        std::fs::write(
            &date,
            r#"#!/bin/sh
[ "$#" -eq 1 ] && [ "$1" = '+%s' ] || exit 2
IFS= read -r now < "$FLOOD_TEST_CLOCK" || exit 3
printf '%s\n' "$now" >> "$FLOOD_TEST_CLOCK_READS" || exit 4
printf '%s\n' "$now"
"#,
        )
        .unwrap();
        std::fs::set_permissions(&date, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = std::env::join_paths(
            std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap();
        let mut producer = Producer(
            std::process::Command::new("/bin/sh")
                .args(["-c", DASHBOARD_FLOOD_PRODUCER, "flood"])
                .arg(&start)
                .arg(&finish)
                .arg(&directory)
                .arg("2")
                .env("PATH", path)
                .env("FLOOD_TEST_CLOCK", &clock)
                .env("FLOOD_TEST_CLOCK_READS", &clock_reads)
                .stdout(std::fs::File::create(&output).unwrap())
                .stderr(std::fs::File::create(&errors).unwrap())
                .spawn()
                .unwrap(),
        );
        let wait = |phase: &str, predicate: &mut dyn FnMut() -> bool| {
            // This watchdog uses real time even if the fixture clock stalls.
            let deadline = Instant::now() + Duration::from_secs(5);
            while !predicate() {
                assert!(
                    Instant::now() < deadline,
                    "{scenario}: timed out {phase}; output bytes: {}; stderr: {}",
                    std::fs::metadata(&output).unwrap().len(),
                    std::fs::read_to_string(&errors).unwrap()
                );
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        // Observe both deadline initialization and the first alive() check
        // before advancing time, even if child startup is delayed by load.
        wait("initializing the clock", &mut || {
            std::fs::read_to_string(&clock_reads)
                .unwrap()
                .lines()
                .count()
                >= 2
        });
        assert!(output_text().is_empty(), "{scenario}: output before start");
        assert!(producer.0.try_wait().unwrap().is_none(), "{scenario}");
        if scenario != "no-start" {
            std::fs::write(&start, b"").unwrap();
            wait("emitting the full flood", &mut || {
                output_text().contains("flood-19999")
            });
            assert!(!output_text().contains("FLOOD-DONE"), "{scenario}");
            assert!(producer.0.try_wait().unwrap().is_none(), "{scenario}");
        }
        match scenario {
            "no-start" | "no-finish" => {
                // Expire at the exact deadline only after the expected output.
                // Rename atomically so date never reads a partially written time.
                let next_clock = root.path().join("next-clock");
                std::fs::write(&next_clock, b"102\n").unwrap();
                std::fs::rename(&next_clock, &clock).unwrap();
            }
            "removed-directory" => std::fs::remove_dir_all(&directory).unwrap(),
            "normal" => std::fs::write(&finish, b"").unwrap(),
            _ => unreachable!(),
        }
        if scenario == "normal" {
            wait("finishing normally", &mut || {
                output_text().contains("FLOOD-DONE")
            });
        } else {
            let mut status = None;
            wait("exiting through the guard", &mut || {
                status = producer.0.try_wait().unwrap();
                status.is_some()
            });
            assert!(!status.unwrap().success(), "{scenario}");
            assert!(!output_text().contains("FLOOD-DONE"), "{scenario}");
            if scenario == "no-start" {
                assert!(output_text().is_empty(), "{scenario}");
            }
        }
    }
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
    let socket = worker.socket_path().unwrap();
    let start = root.path().join("start-flood");
    let finish = root.path().join("finish-flood");
    let producer = root.path().join("flood.sh");
    // Bound both gates and exit if panic removes the fixture directory.
    // The fixture owns output. Never inject input through the inferred client:
    // tmux 3.8 rejects send-keys from Observe even with an explicit pane target.
    std::fs::write(&producer, DASHBOARD_FLOOD_PRODUCER).unwrap();
    let output = std::process::Command::new("tmux")
        .arg("-S")
        .arg(socket)
        .args(["respawn-pane", "-k", "-t", "fluent:0.0", "/bin/sh"])
        .arg(&producer)
        .arg(&start)
        .arg(&finish)
        .arg(root.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut view =
        EmbeddedTerminal::observe(worker.observe(worker.descriptor()).unwrap(), 60, 10).unwrap();
    let clients = || {
        let output = std::process::Command::new("tmux")
            .arg("-S")
            .arg(socket)
            .args([
                "list-clients",
                "-F",
                "#{client_width}x#{client_height}:#{client_readonly}",
            ])
            .output()
            .unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap()
    };
    // The server must confirm Observe before the producer can emit anything.
    wait_for(|| clients().trim() == "60x10:1");
    assert!(!worker.capture_pane_snapshot().unwrap().contains("flood-"));
    std::fs::write(&start, b"").unwrap();
    wait_for(|| {
        view.refresh();
        view.frame().text().contains("flood-")
    });
    fluent::dashboard::test_support::navigate_and_refresh(root.path(), &mut view).unwrap();
    assert_eq!(clients().trim(), "60x10:1");
    assert!(!view.frame().text().contains("FLOOD-DONE"));
    // Keep producing throughout navigation and refresh, then allow completion.
    std::fs::write(&finish, b"").unwrap();
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
    wait_for(|| clients().trim() == "60x10:1");
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
