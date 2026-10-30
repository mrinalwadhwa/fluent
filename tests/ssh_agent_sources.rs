#![cfg(feature = "test-support")]

use fluent::dashboard::project::SourceSnapshot;
use fluent::dashboard_sources::{SshSource, decode_snapshot, encode_snapshot, load_sources};
use fluent::terminal_sessions::{TerminalIdentity, TerminalSession};
use serde_json::Value;

fn source(root: &std::path::Path) -> SshSource {
    SshSource {
        name: "build-mac".into(),
        host: "builder".into(),
        project_root: root.canonicalize().unwrap(),
    }
}

#[test]
fn invalid_remote_protocol_cannot_supply_attachment_authority() {
    let root = tempfile::tempdir().unwrap();
    let source = source(root.path());
    let owner = TerminalSession::start_fixture(
        TerminalIdentity::fixture(root.path(), "work", "attempt-1", "writer"),
        "exec /bin/sleep 30",
    )
    .unwrap();
    let mut snapshot = SourceSnapshot::load(root.path()).unwrap();
    // The wire parser must reject endpoints even on otherwise valid snapshots.
    let bytes = encode_snapshot(&snapshot).unwrap();
    let good: Value = serde_json::from_slice(&bytes).unwrap();
    for (field, value) in [
        ("schema_version", serde_json::json!(999)),
        ("project", serde_json::json!("\u{1b}]52;c;untrusted\u{7}")),
        ("project_root", serde_json::json!("/another/project")),
        ("endpoint", serde_json::json!("/tmp/untrusted.sock")),
        ("command", serde_json::json!("touch /tmp/never-execute")),
    ] {
        let mut bad = good.clone();
        bad[field] = value;
        assert!(
            decode_snapshot(&source, &serde_json::to_vec(&bad).unwrap()).is_err(),
            "{field}"
        );
    }
    assert!(decode_snapshot(&source, &vec![b' '; 2 * 1024 * 1024 + 1]).is_err());
    // Descriptor identity validation is shared by snapshot and attach admission.
    let mut descriptor = owner.descriptor().clone();
    fluent::dashboard_sources::validate_descriptor(&source, &descriptor).unwrap();
    for field in [
        "work_item_id",
        "attempt_id",
        "task_id",
        "execution_generation",
        "session_id",
    ] {
        let mut bad = serde_json::to_value(&descriptor).unwrap();
        bad[field] = serde_json::json!("../escape");
        let bad = serde_json::from_value(bad).unwrap();
        assert!(
            fluent::dashboard_sources::validate_descriptor(&source, &bad).is_err(),
            "{field}"
        );
    }
    descriptor.source_id = "/wrong".into();
    assert!(fluent::dashboard_sources::validate_descriptor(&source, &descriptor).is_err());
    snapshot.id = "/wrong".into();
    assert!(decode_snapshot(&source, &encode_snapshot(&snapshot).unwrap()).is_err());
}

#[test]
fn remote_snapshot_appears_with_source_identity() {
    let root = tempfile::tempdir().unwrap();
    let source = source(root.path());
    fluent::work_model::WorkModelStore::new(root.path())
        .create_work_item(&fluent::work_model::WorkItem::planned(
            "work",
            "Visible project work",
        ))
        .unwrap();
    let snapshot = SourceSnapshot::load(root.path()).unwrap();
    let remote = decode_snapshot(&source, &encode_snapshot(&snapshot).unwrap()).unwrap();
    assert_ne!(remote.id, snapshot.id);
    assert_eq!(remote.id, "ssh:build-mac");
    assert_eq!(remote.host, "builder");
    assert_eq!(remote.path, source.project_root);
    assert_eq!(remote.work[0].key.source_id, source.id());
    let visible = fluent::dashboard::test_support::render_sources(vec![snapshot, remote]);
    assert!(visible.contains("builder"), "{visible}");
    assert!(visible.contains("Visible project work"), "{visible}");
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join(".config/fluent");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("dashboard-sources.json"),
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 1, "ssh": [source]
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(load_sources(home.path()).unwrap().len(), 1);
}

fn fake_ssh(root: &std::path::Path) -> fluent::dashboard_sources::OpenSsh {
    fake_ssh_with_output(root, 0)
}

fn fake_ssh_with_output(
    root: &std::path::Path,
    lines: usize,
) -> fluent::dashboard_sources::OpenSsh {
    fake_ssh_with_client(
        root,
        lines,
        &std::env::current_dir()
            .unwrap()
            .join("tests/fixtures/ssh-terminal-client.py"),
    )
}

fn fake_ssh_with_client(
    root: &std::path::Path,
    lines: usize,
    client: &std::path::Path,
) -> fluent::dashboard_sources::OpenSsh {
    use std::os::unix::fs::PermissionsExt;
    let path = root.join("ssh-fixture");
    // Replace only OpenSSH. Execute the production helper with the exact quoted
    // remote command; no network, provider credentials or host installation.
    let binary = assert_cmd::cargo::cargo_bin("fluent");
    let script = format!(
        r#"#!/usr/bin/env -S python3 -I -S
import os, shlex, sys
args = shlex.split(sys.argv[-1])
os.environ['FLUENT_TEST_TERMINAL_OUTPUT_LINES'] = {lines:?}
if 'terminal' in args:
    import json
    identity = json.loads(args[args.index('--identity') + 1])
    os.environ['FLUENT_TEST_TERMINAL_CLIENT'] = {client:?}
    os.environ['FLUENT_TEST_TERMINAL_CLIENT_LOG'] = os.path.join({root:?}, identity['task_id'] + '.input')
    open(os.path.join({root:?}, 'ssh.pid'), 'w').write(str(os.getpid()))
assert args[:2] == ['fluent', 'agent-source']
os.execv({binary:?}, [{binary:?}] + args[1:])
"#,
        binary = binary.to_string_lossy(),
        lines = lines.to_string(),
        client = client.to_string_lossy(),
        root = root.to_string_lossy()
    );
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    fluent::dashboard_sources::OpenSsh::with_executable(path)
}

#[test]
fn remote_failed_pty_setup_preserves_control_and_reports_cause() {
    use fluent::dashboard_sources::RemoteConnection;
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let mut owner = terminal_fixture(TerminalIdentity::fixture(
        root.path(),
        "remote",
        "attempt-1",
        "writer",
    ))
    .unwrap();
    let client = root.path().join("failing-client");
    let failed = root.path().join("fail-mode");
    let real_client = std::env::current_dir()
        .unwrap()
        .join("tests/fixtures/ssh-terminal-client.py");
    std::fs::write(
        &client,
        format!(
            r#"#!/usr/bin/env -S python3 -I -S
import os, sys
from pathlib import Path
failure = Path({failed:?})
if failure.exists() and failure.read_text() == sys.argv[1]:
    print('PTY_SETUP_SENTINEL: deliberate client failure', file=sys.stderr, flush=True)
    sys.exit(23)
os.execv({real_client:?}, [{real_client:?}] + sys.argv[1:])
"#
        ),
    )
    .unwrap();
    std::fs::set_permissions(&client, std::fs::Permissions::from_mode(0o700)).unwrap();
    let ssh = fake_ssh_with_client(root.path(), 0, &client);
    let mut connection = RemoteConnection::open(
        ssh.clone(),
        source(root.path()),
        owner.descriptor().clone(),
        60,
        10,
    )
    .unwrap();

    std::fs::write(&failed, "control").unwrap();
    let error = connection.acquire_control().unwrap_err();
    eprintln!("expected failed acquisition: {error:#}");
    assert!(
        !connection.has_control(),
        "failed acquisition retained Control"
    );
    assert!(
        error
            .to_string()
            .contains("remote terminal client exited during setup"),
        "{error:#}"
    );
    assert!(
        error.to_string().contains("remote PTY setup pid="),
        "{error:#}"
    );
    assert!(error.to_string().contains("ExitStatus"), "{error:#}");
    assert!(!error.to_string().contains("unsolicited SSH Control change"));
    assert!(
        error
            .to_string()
            .contains("previous_control=false reported_control=false")
    );
    // The error must not poison the transport or strand a newly acquired lease.
    std::fs::remove_file(&failed).unwrap();
    let mut competitor =
        RemoteConnection::open(ssh, source(root.path()), owner.descriptor().clone(), 60, 10)
            .unwrap();
    competitor.acquire_control().unwrap();
    assert!(
        connection
            .acquire_control()
            .unwrap_err()
            .to_string()
            .contains("Control is occupied")
    );
    competitor.observe_only().unwrap();
    connection.acquire_control().unwrap();
    assert!(connection.has_control());

    // Reopening a client must retain an existing lease on failure.
    std::fs::write(&failed, "control").unwrap();
    let error = connection.acquire_control().unwrap_err();
    eprintln!("expected failed reopen: {error:#}");
    assert!(connection.has_control());
    assert!(
        error
            .to_string()
            .contains("previous_control=true reported_control=true")
    );
    assert!(
        error
            .to_string()
            .contains("remote terminal client exited during setup"),
        "{error:#}"
    );
    assert!(competitor.acquire_control().is_err());

    // Observe releases Control before setup: its failed acknowledgement must
    // report that authorized transition and keep the underlying setup error.
    std::fs::write(&failed, "observe").unwrap();
    let error = connection.observe_only().unwrap_err();
    eprintln!("expected failed Observe: {error:#}");
    assert!(!connection.has_control());
    assert!(
        error
            .to_string()
            .contains("previous_control=true reported_control=false")
    );
    assert!(
        error
            .to_string()
            .contains("remote terminal client exited during setup"),
        "{error:#}"
    );
    std::fs::remove_file(&failed).unwrap();
    competitor.acquire_control().unwrap();
    connection.repaint().unwrap();
    connection.poll().unwrap();
    assert!(owner.is_live().unwrap());
    drop(connection);
    drop(competitor);
    owner.stop().unwrap();
}

#[test]
fn snapshot_helper_roundtrip_uses_published_work() {
    let root = tempfile::tempdir().unwrap();
    let source = source(root.path());
    fluent::work_model::WorkModelStore::new(root.path())
        .create_work_item(&fluent::work_model::WorkItem::planned(
            "remote-work",
            "Published remote work",
        ))
        .unwrap();
    let snapshot = fake_ssh(root.path()).snapshot(&source).unwrap();
    assert_eq!(snapshot.work[0].key.source_id, source.id());
    assert_eq!(snapshot.work[0].status.title, "Published remote work");
    assert_eq!(snapshot.work[0].key.work_item_id, "remote-work");
}

#[test]
fn remote_attachment_preserves_invocation_and_control_mode() {
    use fluent::dashboard_sources::RemoteConnection;
    let root = tempfile::tempdir().unwrap();
    let source = source(root.path());
    let mut owner = terminal_fixture(TerminalIdentity::fixture(
        root.path(),
        "remote",
        "attempt-1",
        "writer",
    ))
    .unwrap();
    let descriptor = owner.descriptor().clone();
    let ssh = fake_ssh(root.path());
    let mut client =
        RemoteConnection::open(ssh.clone(), source.clone(), descriptor.clone(), 60, 12).unwrap();
    assert_eq!(client.descriptor(), &descriptor);
    assert!(!client.has_control());
    assert!(client.input(b"forbidden").is_err());
    client.acquire_control().unwrap();
    let mut competitor = RemoteConnection::open(ssh, source, descriptor, 40, 8).unwrap();
    assert!(competitor.acquire_control().is_err());
    assert!(!competitor.has_control());
    client.repaint().unwrap(); // Full-screen and embedded share this connection and lease.
    assert!(client.has_control());
    assert!(competitor.acquire_control().is_err());
    client.observe_only().unwrap();
    competitor.acquire_control().unwrap();
    drop(competitor);
    drop(client);
    assert!(owner.is_live().unwrap());
    owner.stop().unwrap();
}

fn wait_for(mut condition: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
    while !condition() {
        assert!(
            std::time::Instant::now() < deadline,
            "remote terminal condition timed out"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn remote_embedded_input_reaches_only_selected_worker() {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use fluent::terminal_view::EmbeddedTerminal;
    let root = tempfile::tempdir().unwrap();
    let mut first = terminal_fixture(TerminalIdentity::fixture(
        root.path(),
        "remote",
        "attempt-1",
        "first",
    ))
    .unwrap();
    let mut peer = terminal_fixture(TerminalIdentity::fixture(
        root.path(),
        "remote",
        "attempt-1",
        "peer",
    ))
    .unwrap();
    let mut view = EmbeddedTerminal::observe_ssh(
        fake_ssh(root.path()),
        source(root.path()),
        first.descriptor().clone(),
        70,
        12,
    )
    .unwrap();
    assert!(!view.has_control());
    assert!(view.send(Event::Paste("forbidden".into())).is_err());
    view.acquire_control().unwrap();
    wait_for(|| {
        view.refresh();
        view.frame().text().contains('$')
            || view.frame().text().contains('#')
            || view.frame().text().contains('%')
    });
    view.send(Event::Paste("printf 'selected-界\\n'".into()))
        .unwrap();
    view.send(Event::Key(KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::NONE,
    )))
    .unwrap();
    wait_for(|| {
        view.refresh();
        std::fs::read_to_string(root.path().join("first.input"))
            .unwrap_or_default()
            .contains("selected-界")
    });
    assert!(
        !std::fs::read_to_string(root.path().join("peer.input"))
            .unwrap_or_default()
            .contains("selected-界")
    );
    assert!(view.identity().contains("build-mac (builder)"));
    drop(view);
    first.stop().unwrap();
    peer.stop().unwrap();
}

#[test]
fn ssh_fullscreen_fixture_ignores_ambient_startup() {
    assert_fixture_ignores_python_startup("ssh-terminal-client.py", 6);
}

#[test]
fn ssh_fullscreen_helper_ignores_ambient_startup() {
    assert_fixture_ignores_python_startup("ssh-fixture", 9);
}

fn assert_fixture_ignores_python_startup(program: &str, delay: u8) {
    let root = tempfile::tempdir().unwrap();
    // Python startup hooks run before the fixture can enter raw mode. Keep
    // this contamination scoped to a subprocess, never the parallel harness.
    std::fs::write(
        root.path().join("sitecustomize.py"),
        format!(
            r#"import os, sys, time
from pathlib import Path
root = Path(os.environ['FLUENT_SSH_STARTUP_ROOT'])
(root / 'python-startup-ran').touch()
if sys.argv[0].endswith({program:?}):
    time.sleep({delay})
"#
        ),
    )
    .unwrap();
    assert_fixture_starts_and_stops(root.path());
    assert!(!root.path().join("python-startup-ran").exists());
}

#[test]
fn ssh_fullscreen_fixture_owns_its_sleep_process() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let sleep = root.path().join("sleep");
    std::fs::write(
        &sleep,
        r#"#!/usr/bin/env -S python3 -I -S
import os, signal, sys, time
from pathlib import Path
receipt = Path(os.environ['FLUENT_SSH_STARTUP_ROOT'], 'escaped-sleep.pid')
signal.signal(signal.SIGTERM, signal.SIG_IGN)
if os.fork() == 0:
    os.setsid()
    receipt.write_text(str(os.getpid()))
    time.sleep(60)
    os._exit(0)
while not receipt.exists():
    time.sleep(0.01)
os.execl('/bin/sleep', 'sleep', sys.argv[1])
"#,
    )
    .unwrap();
    std::fs::set_permissions(&sleep, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_fixture_starts_and_stops(root.path());
    assert!(!root.path().join("escaped-sleep.pid").exists());
}

fn assert_fixture_starts_and_stops(root: &std::path::Path) {
    let path = std::env::join_paths(
        std::iter::once(root.to_owned())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "ssh_fullscreen_fixture_startup_child",
            "--nocapture",
        ])
        .env("FLUENT_SSH_STARTUP_ROOT", root)
        .env("PYTHONPATH", root)
        .env("PATH", path)
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
fn ssh_fullscreen_fixture_startup_child() {
    let Some(root) = std::env::var_os("FLUENT_SSH_STARTUP_ROOT") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let mut owner = terminal_fixture(TerminalIdentity::fixture(
        &root,
        "remote",
        "attempt-1",
        "writer",
    ))
    .unwrap();
    let result = (|| -> anyhow::Result<()> {
        let mut connection = fluent::dashboard_sources::RemoteConnection::open(
            fake_ssh(&root),
            source(&root),
            owner.descriptor().clone(),
            100,
            30,
        )?;
        connection.acquire_control()?;
        connection.repaint()?;
        connection.observe_only()?;
        Ok(())
    })();
    // Keep the owned generation's cleanup assertion even when startup fails.
    let stopped = owner.stop();
    if let Ok(pid) = std::fs::read_to_string(root.join("escaped-sleep.pid")) {
        // Settle only the descendant created by this test's injected wrapper,
        // including on the expected red path where group cleanup rejects it.
        let pid: i32 = pid.parse().unwrap();
        assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
        wait_for(|| !owner.is_live().unwrap());
        owner.stop().unwrap();
    }
    stopped.unwrap();
    result.unwrap();
    assert!(!owner.is_live().unwrap());
}

#[test]
fn ssh_fullscreen_client_detaches_before_display_drain() {
    let output = std::process::Command::new("python3")
        .args(["-I", "-S"])
        .arg("tests/fixtures/ssh-terminal-client-pressure.py")
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
fn ssh_fullscreen_handoff_keeps_remote_control_lease() {
    for mode in ["observe", "control"] {
        let output = std::process::Command::new("python3")
            .args(["-I", "-S"])
            .arg("tests/fixtures/ssh-terminal-handoff.py")
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
fn ssh_fullscreen_long_reader_stall_preserves_transport() {
    for mode in ["observe", "control"] {
        let output = std::process::Command::new("python3")
            .args(["-I", "-S"])
            .arg("tests/fixtures/ssh-terminal-handoff.py")
            .arg(std::env::current_exe().unwrap())
            .arg(mode)
            .arg("9")
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
fn ssh_fullscreen_stalled_display_remains_bounded() {
    use fluent::dashboard_sources::RemoteConnection;
    use std::time::{Duration, Instant};
    for control in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let mut owner = terminal_fixture(TerminalIdentity::fixture(
            root.path(),
            "remote",
            "attempt-1",
            "writer",
        ))
        .unwrap();
        let mut connection = RemoteConnection::open(
            fake_ssh_with_output(root.path(), 16000),
            source(root.path()),
            owner.descriptor().clone(),
            100,
            30,
        )
        .unwrap();
        if control {
            connection.acquire_control().unwrap();
        }
        // Withhold display consumption while continuing the actual transport.
        let until = Instant::now() + Duration::from_secs(9);
        while Instant::now() < until {
            connection.poll().unwrap();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(connection.has_control(), control);
        assert!(owner.is_live().unwrap());
        let held = connection.take_output();
        assert!(!held.is_empty());
        assert!(held.len() <= 128 * 1024, "pending output: {}", held.len());
        let mut output = held;
        let until = Instant::now() + Duration::from_secs(30);
        while !output.ends_with(b"SSH_FULLSCREEN_PAINT_END\r\n") {
            assert!(Instant::now() < until, "display failed to recover");
            connection.poll().unwrap();
            output.extend(connection.take_output());
            assert!(output.len() < 7 * 1024 * 1024);
            std::thread::sleep(Duration::from_millis(5));
        }
        // The complete final repaint survives; no raw terminal bytes are dropped.
        let marker = b"SSH_FULLSCREEN_PAINT\r\n# ";
        let start = output
            .windows(marker.len())
            .rposition(|w| w == marker)
            .unwrap()
            + marker.len();
        let paint = [vec![b'X'; 98], b"\r\n".to_vec()].concat().repeat(16000);
        assert_eq!(
            &output[start..],
            [paint, b"SSH_FULLSCREEN_PAINT_END\r\n".to_vec()].concat()
        );
        if !control {
            assert!(connection.input(b"forbidden").is_err());
        }
        drop(connection);
        owner.stop().unwrap();
    }
}

#[test]
fn ssh_fullscreen_slow_display_detaches_and_restores() {
    for scenario in ["detach", "finish", "cancel", "disconnect"] {
        for mode in ["observe", "control"] {
            let output = std::process::Command::new("python3")
                .args(["-I", "-S"])
                .arg("tests/fixtures/ssh-terminal-handoff.py")
                .arg(std::env::current_exe().unwrap())
                .args([mode, "9", scenario])
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{mode} {scenario}: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}

#[test]
fn ssh_fullscreen_unread_display_settles_after_detach() {
    assert_unread_display_settles("detach");
}

#[test]
fn ssh_fullscreen_unread_display_settles_after_shutdown() {
    for scenario in ["finish", "cancel", "disconnect"] {
        assert_unread_display_settles(scenario);
    }
}

fn assert_unread_display_settles(scenario: &str) {
    for mode in ["observe", "control"] {
        let output = std::process::Command::new("python3")
            .args(["-I", "-S"])
            .arg("tests/fixtures/ssh-terminal-handoff.py")
            .arg(std::env::current_exe().unwrap())
            .args([mode, "9", scenario, "unread"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{mode} {scenario}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn ssh_fullscreen_partial_input_returns_to_dashboard() {
    for input in [
        "control",
        "partial",
        "terminal-report",
        "fragmented-report",
        "unfinished",
    ] {
        for mode in ["observe", "control"] {
            for scenario in ["drain", "disconnect"] {
                let output = std::process::Command::new("python3")
                    .args(["tests/fixtures/ssh-terminal-handoff.py"])
                    .arg(std::env::current_exe().unwrap())
                    .args([mode, "0.4", scenario])
                    .env("FLUENT_SSH_PARTIAL_INPUT", input)
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "{mode} {scenario} {input}: {}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }
    }
}

#[test]
fn ssh_fullscreen_pty_child() {
    let Ok(mode) = std::env::var("FLUENT_SSH_FULLSCREEN_CHILD") else {
        return;
    };
    use fluent::dashboard_sources::RemoteConnection;
    use fluent::terminal_view::EmbeddedTerminal;
    let root = tempfile::tempdir().unwrap();
    let mut owner = terminal_fixture(TerminalIdentity::fixture(
        root.path(),
        "remote",
        "attempt-1",
        "writer",
    ))
    .unwrap();
    let scenario =
        std::env::var("FLUENT_SSH_FULLSCREEN_SCENARIO").unwrap_or_else(|_| "drain".into());
    let signals = std::env::var_os("FLUENT_SSH_FULLSCREEN_SIGNALS").map(std::path::PathBuf::from);
    let ssh = fake_ssh_with_output(root.path(), if scenario == "drain" { 0 } else { 16000 });
    let config = source(root.path());
    eprintln!(
        "SSH_PHASE initial-attachment generation={}",
        owner.descriptor().execution_generation
    );
    let mut view = EmbeddedTerminal::observe_ssh(
        ssh.clone(),
        config.clone(),
        owner.descriptor().clone(),
        60,
        10,
    )
    .unwrap();
    if mode == "control" {
        view.acquire_control().unwrap();
    }
    let helper_pid: i32 = std::fs::read_to_string(root.path().join("ssh.pid"))
        .unwrap()
        .parse()
        .unwrap();
    let original = owner.descriptor().clone();
    eprintln!("SSH_PHASE initial-attached control={}", view.has_control());
    let mut terminal = fluent::dashboard::terminal::DashboardTerminal::open().unwrap();
    // Initialize the dashboard event reader before handing stdin to full-screen.
    // The real dashboard enters this path after reading the operator's f key.
    println!("SSH_DASHBOARD_READY");
    assert!(crossterm::event::poll(std::time::Duration::from_secs(5)).unwrap());
    assert!(matches!(
        crossterm::event::read().unwrap(),
        crossterm::event::Event::Key(key) if key.code == crossterm::event::KeyCode::Char('f')
    ));
    // A separate process probes the real remote lease while full-screen IO runs.
    eprintln!("SSH_PHASE competitor-attachment");
    let mut competitor = RemoteConnection::open(
        ssh.clone(),
        config.clone(),
        owner.descriptor().clone(),
        40,
        8,
    )
    .unwrap();
    if mode == "control" {
        assert!(competitor.acquire_control().is_err());
    }
    drop(competitor);
    eprintln!("SSH_PHASE competitor-settled");
    let stdin_flags = unsafe { libc::fcntl(0, libc::F_GETFL) };
    let stdout_flags = unsafe { libc::fcntl(1, libc::F_GETFL) };
    let finished = std::sync::atomic::AtomicBool::new(false);
    let (result, returned_owner) = std::thread::scope(|scope| {
        let lifecycle = scope.spawn(|| {
            while !finished.load(std::sync::atomic::Ordering::SeqCst) {
                if signals.as_ref().is_some_and(|p| p.join("cleanup").exists()) {
                    owner.stop().unwrap();
                    std::fs::write(signals.as_ref().unwrap().join("cleanup-done"), b"settled")
                        .unwrap();
                    break;
                }
                if signals.as_ref().is_some_and(|p| p.join("trigger").exists()) {
                    match scenario.as_str() {
                        "finish" => owner.finish().unwrap(),
                        "cancel" => owner.stop().unwrap(),
                        "disconnect" => {
                            assert_eq!(unsafe { libc::kill(helper_pid, libc::SIGKILL) }, 0)
                        }
                        _ => panic!("unexpected lifecycle action"),
                    }
                    std::fs::write(signals.as_ref().unwrap().join("settled"), b"settled").unwrap();
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            owner
        });
        let result = terminal.handoff(|| view.fullscreen(100, 30));
        finished.store(true, std::sync::atomic::Ordering::SeqCst);
        (result, lifecycle.join().unwrap())
    });
    owner = returned_owner;
    if std::env::var_os("FLUENT_SSH_FULLSCREEN_UNREAD").is_some() {
        // No diagnostic or assertion-success output may unblock this path. The
        // parent keeps the PTY unread until all local attachment cleanup ends.
        assert!(result.is_err());
        assert!(!terminal.is_active());
        assert_eq!(view.descriptor(), &original);
        assert!(!view.has_control());
        assert!(!crossterm::terminal::is_raw_mode_enabled().unwrap());
        assert_eq!(unsafe { libc::fcntl(0, libc::F_GETFL) }, stdin_flags);
        assert_eq!(unsafe { libc::fcntl(1, libc::F_GETFL) }, stdout_flags);
        assert_eq!(unsafe { libc::kill(helper_pid, 0) }, -1);
        if matches!(scenario.as_str(), "finish" | "cancel") {
            assert!(view.ended());
            assert!(!owner.is_live().unwrap());
            assert!(view.reconnect().is_err());
        } else {
            assert!(owner.is_live().unwrap(), "display cleanup canceled worker");
            // The original invocation survives and its old lease is released.
            let mut competitor = RemoteConnection::open(ssh, config, original, 40, 8).unwrap();
            competitor.acquire_control().unwrap();
            drop(competitor);
        }
        drop(view);
        drop(terminal);
        owner.stop().unwrap();
        assert!(!owner.is_live().unwrap());
        std::fs::write(signals.unwrap().join("local-settled"), b"settled").unwrap();
        return;
    }
    println!("SSH_FULLSCREEN_RETURNED");
    if std::env::var_os("FLUENT_SSH_PARTIAL_INPUT").is_some() {
        fluent::dashboard::test_support::assert_handoff_navigation().unwrap();
    }
    if scenario == "disconnect" {
        assert!(result.is_err());
    } else {
        assert_eq!(result.unwrap(), 0);
    }
    assert_eq!(view.descriptor(), &original);
    assert!(crossterm::terminal::is_raw_mode_enabled().unwrap());
    assert_eq!(unsafe { libc::fcntl(0, libc::F_GETFL) }, stdin_flags);
    assert_eq!(unsafe { libc::fcntl(1, libc::F_GETFL) }, stdout_flags);
    if matches!(scenario.as_str(), "finish" | "cancel") {
        assert!(view.ended());
        assert!(!view.has_control());
        assert!(view.acquire_control().is_err());
        assert!(
            view.send(crossterm::event::Event::Paste("ended".into()))
                .is_err()
        );
        assert!(!owner.is_live().unwrap());
        let mut replacement = terminal_fixture(TerminalIdentity::fixture(
            root.path(),
            "remote",
            "attempt-1",
            "writer",
        ))
        .unwrap();
        assert_ne!(
            replacement.descriptor().execution_generation,
            original.execution_generation
        );
        assert!(view.reconnect().is_err());
        assert_eq!(view.descriptor(), &original);
        assert!(replacement.is_live().unwrap());
        replacement.stop().unwrap();
    } else {
        assert!(owner.is_live().unwrap());
        if scenario == "disconnect" {
            assert!(!view.has_control());
            assert!(
                view.send(crossterm::event::Event::Paste("disconnected".into()))
                    .is_err()
            );
            view.reconnect().unwrap();
            assert!(!view.has_control());
        } else {
            view.refresh();
            assert!(
                !view.display().state.contains("Disconnected"),
                "{}",
                view.state_label()
            );
            assert_eq!(view.has_control(), mode == "control");
        }
        // A fresh competitor distinguishes lease rejection from timeout.
        eprintln!("SSH_PHASE post-handoff-competitor");
        let mut competitor = RemoteConnection::open(ssh, config, original, 40, 8).unwrap();
        if mode == "control" && scenario != "disconnect" {
            assert!(
                competitor
                    .acquire_control()
                    .unwrap_err()
                    .to_string()
                    .contains("occupied")
            );
        } else {
            competitor.acquire_control().unwrap();
        }
        drop(competitor);
        drop(view);
        owner.stop().unwrap();
    }
    drop(terminal);
    assert!(!crossterm::terminal::is_raw_mode_enabled().unwrap());
}

fn terminal_fixture(identity: TerminalIdentity) -> anyhow::Result<TerminalSession> {
    TerminalSession::start_fixture(identity, "exec /bin/sleep 60")
}

#[test]
fn ssh_disconnect_discards_unsent_input() {
    use crossterm::event::Event;
    use fluent::terminal_view::EmbeddedTerminal;
    let root = tempfile::tempdir().unwrap();
    let mut owner = terminal_fixture(TerminalIdentity::fixture(
        root.path(),
        "remote",
        "attempt-1",
        "writer",
    ))
    .unwrap();
    let mut view = EmbeddedTerminal::observe_ssh(
        fake_ssh(root.path()),
        source(root.path()),
        owner.descriptor().clone(),
        60,
        10,
    )
    .unwrap();
    view.acquire_control().unwrap();
    // Queue input locally and break the connection before the next IO poll.
    view.send(Event::Paste("must-not-replay".into())).unwrap();
    let pid: i32 = std::fs::read_to_string(root.path().join("ssh.pid"))
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
    wait_for(|| {
        view.refresh();
        view.state_label().contains("Disconnected")
    });
    assert!(!view.has_control());
    assert!(!view.focused());
    assert!(owner.is_live().unwrap());
    view.reconnect().unwrap();
    assert_eq!(view.descriptor(), owner.descriptor());
    assert!(!view.has_control());
    assert!(view.send(Event::Paste("still-observe".into())).is_err());
    view.acquire_control().unwrap();
    view.send(Event::Paste("fresh-input".into())).unwrap();
    wait_for(|| {
        view.refresh();
        std::fs::read_to_string(root.path().join("writer.input"))
            .unwrap_or_default()
            .contains("fresh-input")
    });
    let received = std::fs::read_to_string(root.path().join("writer.input")).unwrap();
    assert!(!received.contains("must-not-replay"));
    assert!(!received.contains("still-observe"));
    drop(view);
    owner.stop().unwrap();
}

#[test]
fn reconnect_never_restarts_or_retargets_worker() {
    use fluent::terminal_view::EmbeddedTerminal;
    let root = tempfile::tempdir().unwrap();
    let identity = TerminalIdentity::fixture(root.path(), "remote", "attempt-1", "writer");
    let mut owner = terminal_fixture(identity.clone()).unwrap();
    let original = owner.descriptor().clone();
    let mut view = EmbeddedTerminal::observe_ssh(
        fake_ssh(root.path()),
        source(root.path()),
        original.clone(),
        60,
        10,
    )
    .unwrap();
    owner.stop().unwrap();
    let mut replacement = terminal_fixture(identity).unwrap();
    let replacement_descriptor = replacement.descriptor().clone();
    assert_ne!(
        replacement_descriptor.execution_generation,
        original.execution_generation
    );
    assert!(view.reconnect().is_err());
    assert!(
        view.state_label().contains("Disconnected"),
        "{}",
        view.state_label()
    );
    assert_eq!(view.descriptor(), &original);
    assert!(!view.has_control());
    assert!(view.acquire_control().is_err());
    assert_eq!(replacement.descriptor(), &replacement_descriptor);
    assert!(replacement.is_live().unwrap());
    drop(view);
    replacement.stop().unwrap();
}

#[test]
fn ssh_prerequisite_failure_isolated_to_source() {
    use fluent::dashboard_sources::RemoteSources;
    let root = tempfile::tempdir().unwrap();
    let ssh = fake_ssh(root.path());
    let script = root.path().join("ssh-fixture");
    let code = std::fs::read_to_string(&script).unwrap().replace("args = shlex.split(sys.argv[-1])", r#"
errors = {'auth': 'Permission denied (publickey)', 'host-key': 'Host key verification failed', 'prerequisite': 'fluent: command not found'}
if sys.argv[-2] in errors:
    print(errors[sys.argv[-2]], file=sys.stderr)
    sys.exit(255)
args = shlex.split(sys.argv[-1])"#);
    std::fs::write(script, code).unwrap();
    fluent::work_model::WorkModelStore::new(root.path())
        .create_work_item(&fluent::work_model::WorkItem::planned(
            "live",
            "Healthy project",
        ))
        .unwrap();
    let mut sources = vec![source(root.path())];
    for name in ["auth", "host-key", "prerequisite"] {
        let mut config = source(root.path());
        config.name = name.into();
        config.host = name.into();
        sources.push(config);
    }
    let mut remote = RemoteSources::new(sources, ssh);
    remote.refresh();
    // Poll remains nonblocking while every host has its own outstanding request.
    let start = std::time::Instant::now();
    remote.poll();
    assert!(start.elapsed() < std::time::Duration::from_millis(100));
    wait_for(|| {
        remote.poll();
        let snapshots = remote.snapshots();
        snapshots[0].stale.is_none()
            && snapshots[1..].iter().all(|s| {
                s.stale
                    .as_deref()
                    .is_some_and(|e| e.contains("establish SSH access"))
            })
    });
    let mut all = vec![SourceSnapshot::load(root.path()).unwrap()];
    all.extend(remote.snapshots());
    assert_eq!(all[0].work[0].status.title, "Healthy project");
    assert_eq!(all[1].work[0].status.title, "Healthy project");
    for snapshot in &all[2..] {
        let error = snapshot.stale.as_ref().unwrap();
        assert!(error.contains(&snapshot.host));
        assert!(error.contains("outside the dashboard"));
        assert!(error.contains("canonical project root"));
    }
}

#[test]
fn ssh_source_uses_existing_host_without_provider_credentials() {
    use fluent::dashboard_sources::OpenSsh;
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("project ' $(touch INJECTED) ; name");
    std::fs::create_dir(&root).unwrap();
    let config = source(&root);
    let ssh = OpenSsh::default();
    let command = ssh.command(&config, "snapshot", &[], false).unwrap();
    let args: Vec<_> = command
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    for option in [
        "BatchMode=yes",
        "StrictHostKeyChecking=yes",
        "ForwardAgent=no",
        "ClearAllForwardings=yes",
        "PermitLocalCommand=no",
    ] {
        assert!(args.iter().any(|a| a == option));
    }
    assert_eq!(args[args.len() - 2], config.host);
    let remote_command = args.last().unwrap();
    for forbidden in [
        "CODEX_HOME",
        "ANTHROPIC_API_KEY",
        "OPENAI_API_KEY",
        "scp ",
        "aws ",
        "SendEnv",
    ] {
        assert!(!remote_command.contains(forbidden));
    }
    // Exercise the real shell boundary with metacharacters in the configured root.
    let shim = temp.path().join("ssh-shell");
    let bin_dir = assert_cmd::cargo::cargo_bin("fluent")
        .parent()
        .unwrap()
        .to_owned();
    let body = format!(
        "#!/bin/sh\nfor last do :; done\nPATH='{}':\"$PATH\" exec /bin/sh -c \"$last\"\n",
        bin_dir.display()
    );
    std::fs::write(&shim, body).unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        OpenSsh::with_executable(shim)
            .snapshot(&config)
            .unwrap()
            .path,
        config.project_root
    );
    assert!(!std::path::Path::new("INJECTED").exists());
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.join(".fluent")).unwrap();
    std::fs::write(
        root.join(".fluent/dashboard-sources.json"),
        b"untrusted project configuration",
    )
    .unwrap();
    assert!(load_sources(home.path()).unwrap().is_empty());
    let mut invalid = config;
    invalid.host = "builder;touch INJECTED".into();
    assert!(ssh.command(&invalid, "snapshot", &[], false).is_err());
}

#[test]
fn remote_protocol_rejects_unsolicited_control() {
    use fluent::dashboard_sources::{OpenSsh, RemoteConnection};
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let owner = terminal_fixture(TerminalIdentity::fixture(
        root.path(),
        "remote",
        "attempt-1",
        "writer",
    ))
    .unwrap();
    let messages = format!(
        "{}\n{}\n",
        serde_json::json!({"kind":"ready","schema_version":2,"descriptor":owner.descriptor()}),
        serde_json::json!({"kind":"ack","id":0,"control":true,"error":null})
    );
    let shim = root.path().join("unsolicited-helper");
    std::fs::write(&shim, format!("#!/usr/bin/env -S python3 -I -S\nimport sys,time\nsys.stdout.write({messages:?})\nsys.stdout.flush()\ntime.sleep(1)\n")).unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o700)).unwrap();
    let result = RemoteConnection::open(
        OpenSsh::with_executable(shim),
        source(root.path()),
        owner.descriptor().clone(),
        60,
        10,
    );
    match result {
        Err(_) => {}
        Ok(mut connection) => {
            assert!(connection.poll().is_err());
            assert!(!connection.has_control());
        }
    }
}

#[test]
fn remote_helper_disconnect_retains_complete_bounded_stderr() {
    use fluent::dashboard_sources::{OpenSsh, RemoteConnection};
    use std::os::unix::fs::PermissionsExt;
    for length in [8 * 1024, 16 * 1024, 16 * 1024 + 1] {
        let root = tempfile::tempdir().unwrap();
        let mut owner = terminal_fixture(TerminalIdentity::fixture(
            root.path(),
            "remote",
            "attempt-1",
            "writer",
        ))
        .unwrap();
        let shim = root.path().join("failed-helper");
        std::fs::write(
            &shim,
            format!(
                r#"#!/usr/bin/env -S python3 -I -S
import os
tail = b'HELPER_ERROR_TAIL\x1b[31m\n'
message = b'x' * ({length} - len(tail)) + tail
while message:
    message = message[os.write(2, message):]
os._exit(23)
"#
            ),
        )
        .unwrap();
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o700)).unwrap();
        let result = RemoteConnection::open(
            OpenSsh::with_executable(shim),
            source(root.path()),
            owner.descriptor().clone(),
            60,
            10,
        );
        let Err(error) = result else {
            panic!("failed helper admitted")
        };
        let message = format!("{error:#}");
        if length <= 16 * 1024 {
            assert!(
                message.contains("HELPER_ERROR_TAIL"),
                "stderr tail was lost: {message}"
            );
            assert_eq!(message.matches("HELPER_ERROR_TAIL").count(), 1);
            assert!(!message.contains('\x1b'), "unsanitized helper diagnostic");
        } else {
            assert!(
                message.contains("SSH error output exceeds bound"),
                "{message}"
            );
        }
        assert!(owner.is_live().unwrap());
        owner.stop().unwrap();
    }
}

#[test]
fn remote_startup_timeout_retains_helper_diagnostics() {
    use fluent::dashboard_sources::{OpenSsh, RemoteConnection};
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let mut owner = terminal_fixture(TerminalIdentity::fixture(
        root.path(),
        "remote",
        "attempt-1",
        "writer",
    ))
    .unwrap();
    let shim = root.path().join("stalled-helper");
    std::fs::write(&shim, "#!/usr/bin/env -S python3 -I -S\nimport sys,time\nprint('STARTUP_SENTINEL: waiting for PTY raw mode', file=sys.stderr, flush=True)\ntime.sleep(30)\n").unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o700)).unwrap();
    let result = RemoteConnection::open(
        OpenSsh::with_executable(shim),
        source(root.path()),
        owner.descriptor().clone(),
        60,
        10,
    );
    let Err(error) = result else {
        panic!("stalled helper admitted")
    };
    let message = format!("{error:#}");
    eprintln!("expected helper timeout: {message}");
    assert!(message.contains("SSH attachment timed out"), "{message}");
    assert!(message.contains("STARTUP_SENTINEL"), "{message}");
    assert!(message.contains("ready=false"), "{message}");
    assert!(message.contains("control=false"), "{message}");
    assert!(message.contains("elapsed="), "{message}");
    owner.stop().unwrap();
}

#[test]
fn remote_initial_raw_mode_timeout_retains_pty_diagnostics() {
    use fluent::dashboard_sources::RemoteConnection;
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let mut owner = terminal_fixture(TerminalIdentity::fixture(
        root.path(),
        "remote",
        "attempt-1",
        "writer",
    ))
    .unwrap();
    let client = root.path().join("stalled-client");
    std::fs::write(&client, "#!/usr/bin/env -S python3 -I -S\nimport sys,time\nprint('RAW_SETUP_SENTINEL', file=sys.stderr, flush=True)\ntime.sleep(30)\n").unwrap();
    std::fs::set_permissions(&client, std::fs::Permissions::from_mode(0o700)).unwrap();
    let result = RemoteConnection::open(
        fake_ssh_with_client(root.path(), 0, &client),
        source(root.path()),
        owner.descriptor().clone(),
        60,
        10,
    );
    let Err(error) = result else {
        panic!("non-raw client admitted")
    };
    let message = format!("{error:#}");
    eprintln!("expected PTY timeout: {message}");
    for expected in [
        "did not enter raw mode",
        "RAW_SETUP_SENTINEL",
        "pid=",
        "raw_wait=",
        "lflag=Some(",
        "ready=false",
    ] {
        assert!(message.contains(expected), "missing {expected}: {message}");
    }
    assert!(owner.is_live().unwrap());
    owner.stop().unwrap();
}

#[test]
fn remote_protocol_rejects_unrequested_control_changes_on_error() {
    use fluent::dashboard_sources::{OpenSsh, RemoteConnection};
    use std::os::unix::fs::PermissionsExt;
    for control in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let mut owner = terminal_fixture(TerminalIdentity::fixture(
            root.path(),
            "remote",
            "attempt-1",
            "writer",
        ))
        .unwrap();
        let shim = root.path().join("invalid-control-helper");
        let ready =
            serde_json::json!({"kind":"ready","schema_version":2,"descriptor":owner.descriptor()})
                .to_string();
        std::fs::write(&shim, format!(r#"#!/usr/bin/env -S python3 -I -S
import json, sys
print({ready:?}, flush=True)
state = False
for line in sys.stdin:
    request = json.loads(line)
    action = request['action']['kind']
    if action == 'control':
        state, error = True, None
    elif action == 'repaint':
        state, error = {invalid}, 'deliberate repaint failure'
    else:
        error = None
    print(json.dumps({{'kind':'ack', 'id':request['id'], 'control':state, 'error':error}}), flush=True)
"#, invalid=if control { "False" } else { "True" })).unwrap();
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut connection = RemoteConnection::open(
            OpenSsh::with_executable(shim),
            source(root.path()),
            owner.descriptor().clone(),
            60,
            10,
        )
        .unwrap();
        if control {
            connection.acquire_control().unwrap();
        }
        let error = connection.repaint().unwrap_err();
        assert!(
            error.to_string().contains("unsolicited SSH Control change"),
            "{error:#}"
        );
        assert!(!connection.has_control());
        drop(connection);
        owner.stop().unwrap();
    }
}

#[test]
fn ssh_fullscreen_rejects_uncredited_output_and_old_helpers() {
    use fluent::dashboard_sources::{OpenSsh, RemoteConnection};
    use std::os::unix::fs::PermissionsExt;
    for version in [1, 2] {
        let root = tempfile::tempdir().unwrap();
        let mut owner = terminal_fixture(TerminalIdentity::fixture(
            root.path(),
            "remote",
            "attempt-1",
            "writer",
        ))
        .unwrap();
        let shim = root.path().join("bad-helper");
        let identity = serde_json::to_string(owner.descriptor()).unwrap();
        let script = format!(
            r#"#!/usr/bin/env -S python3 -I -S
import json, sys, time
print(json.dumps({{'kind': 'ready', 'schema_version': {version}, 'descriptor': json.loads({identity:?})}}))
if {version} == 2:
    for _ in range(17):
        print(json.dumps({{'kind': 'output', 'bytes': [88] * 8192}}))
sys.stdout.flush()
time.sleep(5)
"#
        );
        std::fs::write(&shim, script).unwrap();
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o700)).unwrap();
        let opened = RemoteConnection::open(
            OpenSsh::with_executable(shim),
            source(root.path()),
            owner.descriptor().clone(),
            60,
            10,
        );
        if version == 1 {
            let Err(error) = opened else {
                panic!("old terminal helper admitted");
            };
            assert!(
                format!("{error:#}").contains("protocol changed"),
                "{error:#}"
            );
        } else {
            let mut connection = opened.unwrap();
            let mut failure = None;
            wait_for(|| {
                failure = connection.poll().err();
                failure.is_some()
            });
            assert!(
                failure
                    .unwrap()
                    .to_string()
                    .contains("display output exceeds bound")
            );
            assert!(!connection.has_control());
        }
        assert!(owner.is_live().unwrap());
        owner.stop().unwrap();
    }
}
