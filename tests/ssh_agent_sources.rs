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
        "sleep 30",
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
    use std::os::unix::fs::PermissionsExt;
    let path = root.join("ssh-fixture");
    // Replace only OpenSSH. Execute the production helper with the exact quoted
    // remote command; no network, provider credentials or host installation.
    let binary = assert_cmd::cargo::cargo_bin("fluent");
    let script = format!(
        r#"#!/usr/bin/env python3
import os, shlex, sys
args = shlex.split(sys.argv[-1])
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
        client = std::env::current_dir()
            .unwrap()
            .join("tests/fixtures/ssh-terminal-client.py")
            .to_string_lossy(),
        root = root.to_string_lossy()
    );
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    fluent::dashboard_sources::OpenSsh::with_executable(path)
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
fn ssh_fullscreen_handoff_keeps_remote_control_lease() {
    for mode in ["observe", "control"] {
        let output = std::process::Command::new("python3")
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
    let ssh = fake_ssh(root.path());
    let config = source(root.path());
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
    let mut competitor =
        RemoteConnection::open(ssh, config, owner.descriptor().clone(), 40, 8).unwrap();
    if mode == "control" {
        assert!(competitor.acquire_control().is_err());
    }
    let stdin_flags = unsafe { libc::fcntl(0, libc::F_GETFL) };
    let stdout_flags = unsafe { libc::fcntl(1, libc::F_GETFL) };
    assert_eq!(terminal.handoff(|| view.fullscreen(100, 30)).unwrap(), 0);
    println!("SSH_FULLSCREEN_RETURNED");
    assert!(!view.display().state.contains("Disconnected"));
    assert_eq!(view.descriptor(), owner.descriptor());
    assert!(crossterm::terminal::is_raw_mode_enabled().unwrap());
    assert_eq!(unsafe { libc::fcntl(0, libc::F_GETFL) }, stdin_flags);
    assert_eq!(unsafe { libc::fcntl(1, libc::F_GETFL) }, stdout_flags);
    assert_eq!(view.has_control(), mode == "control");
    if mode == "control" {
        assert!(competitor.acquire_control().is_err());
    }
    drop(competitor);
    drop(view);
    assert!(owner.is_live().unwrap());
    owner.stop().unwrap();
}

fn terminal_fixture(identity: TerminalIdentity) -> anyhow::Result<TerminalSession> {
    TerminalSession::start_fixture(identity, "sleep 60")
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
        serde_json::json!({"kind":"ready","schema_version":1,"descriptor":owner.descriptor()}),
        serde_json::json!({"kind":"ack","id":0,"control":true,"error":null})
    );
    let shim = root.path().join("unsolicited-helper");
    std::fs::write(&shim, format!("#!/usr/bin/env python3\nimport sys,time\nsys.stdout.write({messages:?})\nsys.stdout.flush()\ntime.sleep(1)\n")).unwrap();
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
