#![cfg(feature = "test-support")]

use fluent::terminal_sessions::{TerminalIdentity, TerminalSession};
use fluent::terminal_view::{EmbeddedTerminal, Screen};
use std::time::{Duration, Instant};

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
