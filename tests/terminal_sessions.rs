#![cfg(feature = "test-support")]

use fluent::terminal_sessions::{ControlOutcome, TerminalIdentity, TerminalSession};

#[test]
fn terminal_is_bound_before_attachment() {
    let root = tempfile::tempdir().unwrap();
    let identity = TerminalIdentity::fixture(root.path(), "work-1", "attempt-1", "task-1");
    let mut session = TerminalSession::start_fixture(identity, "sleep 30").unwrap();
    let descriptor = session.descriptor().clone();
    assert_eq!(descriptor.work_item_id, "work-1");
    assert_eq!(descriptor.attempt_id, "attempt-1");
    assert_eq!(descriptor.task_id, "task-1");
    assert_eq!(
        fluent::terminal_sessions::list(root.path()).unwrap(),
        vec![descriptor.clone()]
    );
    let output = assert_cmd::Command::cargo_bin("fluent")
        .unwrap()
        .args([
            "terminal",
            "list",
            "--source",
            root.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains(&descriptor.session_id));
    let attachment = session.observe(&descriptor).unwrap();
    attachment.detach();
    let second = session.observe(&descriptor).unwrap();
    second.detach();
    session.stop().unwrap();
}

#[test]
fn detach_does_not_cancel_owned_worker() {
    let root = tempfile::tempdir().unwrap();
    let identity = TerminalIdentity::fixture(root.path(), "work-1", "attempt-1", "task-1");
    let mut session = TerminalSession::start_fixture(identity, "sleep 30").unwrap();
    let descriptor = session.descriptor().clone();
    session.observe(&descriptor).unwrap().detach();
    assert!(session.is_live().unwrap());
    session.stop().unwrap();
}

#[test]
fn stale_terminal_identity_cannot_attach_or_signal() {
    let root = tempfile::tempdir().unwrap();
    let identity = TerminalIdentity::fixture(root.path(), "work-1", "attempt-1", "task-1");
    let record_path = identity.artifact_dir.join("execution.json");
    let mut session = TerminalSession::start_fixture(identity, "sleep 30").unwrap();
    let descriptor = session.descriptor().clone();
    let original = std::fs::read(&record_path).unwrap();
    let mut record: serde_json::Value = serde_json::from_slice(&original).unwrap();
    record["launch_generation"] = "changed".into();
    std::fs::write(&record_path, serde_json::to_vec(&record).unwrap()).unwrap();
    assert!(session.observe(&descriptor).is_err());
    assert!(session.is_live().unwrap());
    std::fs::write(&record_path, original).unwrap();
    session.stop().unwrap();
}

#[test]
fn terminal_commands_preserve_role_confinement() {
    let root = tempfile::tempdir().unwrap();
    let resolver = fluent::content::ContentResolver::new(Some(root.path()));
    let profile = fluent::os::render_profile_common_only(
        &resolver,
        root.path().to_str().unwrap(),
        &[root.path().join("worker")],
        &[],
    )
    .unwrap();
    let socket_dir = root.path().join("socket-dir");
    std::fs::create_dir(&socket_dir).unwrap();
    let terminal_profile = fluent::os::profile_with_terminal_socket(&profile, &socket_dir).unwrap();
    let content = std::fs::read_to_string(&terminal_profile.path).unwrap();
    assert!(content.contains(&format!("(subpath \"{}\")", socket_dir.display())));
    assert!(!content.contains("(allow file-write* (subpath \"/private/tmp\"))"));
    assert!(
        content.contains(
            "(deny network-outbound (local unix-socket (path-prefix \"/private/tmp/\")))"
        )
    );
}

#[test]
fn second_controller_is_observer_until_control_is_released() {
    let root = tempfile::tempdir().unwrap();
    let identity = TerminalIdentity::fixture(root.path(), "work-1", "attempt-1", "task-1");
    let mut session = TerminalSession::start_fixture(identity, "sleep 30").unwrap();
    let descriptor = session.descriptor().clone();
    let mut first = session.observe(&descriptor).unwrap();
    let mut second = session.observe(&descriptor).unwrap();
    assert!(
        second
            .resize(80, 24)
            .unwrap_err()
            .to_string()
            .contains("Control")
    );
    assert_eq!(first.acquire_control().unwrap(), ControlOutcome::Acquired);
    assert_eq!(second.acquire_control().unwrap(), ControlOutcome::Occupied);
    assert_eq!(second.descriptor(), &descriptor);
    first.detach();
    assert_eq!(second.acquire_control().unwrap(), ControlOutcome::Acquired);
    second.detach();
    session.stop().unwrap();
}

#[test]
fn unavailable_terminal_has_specific_reason() {
    use fluent::terminal_sessions::unavailable_reason;
    use fluent::work_model::{TaskKind, TaskStatus};
    assert!(unavailable_reason(TaskKind::Write, TaskStatus::Planned, false).contains("legacy"));
    assert!(unavailable_reason(TaskKind::Write, TaskStatus::Executing, true).contains("headless"));
    assert!(unavailable_reason(TaskKind::Write, TaskStatus::Complete, true).contains("completed"));
    assert!(unavailable_reason(TaskKind::Tester, TaskStatus::Executing, true).contains("Tester"));
}

#[test]
fn settlement_covers_terminal_process_scope() {
    let table = "10 1 10\n20 10 20\n21 20 21\n22 21 22\n30 1 30\n";
    let owned = fluent::terminal_sessions::descendants_from_ps(table, &[20]).unwrap();
    assert_eq!(owned, vec![20, 21, 22]);
    assert!(!owned.contains(&30));
}

#[test]
fn terminal_descriptor_v1_fixture_round_trips() {
    let value = include_str!("fixtures/terminal-descriptor-v1.json");
    let descriptor: fluent::terminal_sessions::TerminalDescriptor =
        serde_json::from_str(value).unwrap();
    assert_eq!(descriptor.schema_version, 1);
    assert_eq!(
        descriptor.lifecycle,
        fluent::terminal_sessions::TerminalLifecycle::Running
    );
    let round_trip = serde_json::to_value(&descriptor).unwrap();
    assert_eq!(
        round_trip,
        serde_json::from_str::<serde_json::Value>(value).unwrap()
    );
}

#[test]
fn partial_terminal_setup_cleans_only_its_resources() {
    let root = tempfile::tempdir().unwrap();
    let first = TerminalIdentity::fixture(root.path(), "work-1", "attempt-1", "task-1");
    let second = TerminalIdentity::fixture(root.path(), "work-1", "attempt-1", "task-2");
    let mut live = TerminalSession::start_fixture(first, "sleep 30").unwrap();
    assert!(TerminalSession::start_fixture_failed_setup(second).is_err());
    assert!(live.is_live().unwrap());
    live.stop().unwrap();
}

#[test]
fn host_managed_terminal_acceptance() {
    if std::env::var_os("FLUENT_TEST_REAL_TERMINAL_HOST").is_none() {
        return;
    }
    use fluent::work_model::{WorkItem, WorkModelStore};
    use std::path::Path;
    use std::process::Command;
    use std::time::{Duration, Instant};

    fn wait_file(path: &Path) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !path.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(30));
        }
        assert!(path.exists(), "{} was not created", path.display());
    }
    fn send(socket: &Path, command: &str) {
        assert!(
            Command::new("tmux")
                .arg("-S")
                .arg(socket)
                .args(["send-keys", "-l", command])
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("tmux")
                .arg("-S")
                .arg(socket)
                .args(["send-keys", "Enter"])
                .status()
                .unwrap()
                .success()
        );
    }
    fn group(pid: u32) -> String {
        String::from_utf8(
            Command::new("/bin/ps")
                .args(["-o", "pgid=", "-p", &pid.to_string()])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string()
    }
    let root = tempfile::tempdir_in("/private/tmp").unwrap();
    let source = root.path().canonicalize().unwrap();
    let store = WorkModelStore::new(&source);
    for work in ["work-1", "work-2"] {
        let mut item = WorkItem::planned(work, "Terminal host fixture");
        item.add_initial_attempt("attempt-1").unwrap();
        store.create_work_item(&item).unwrap();
    }
    let worker1 = source.join("worker-1");
    let worker2 = source.join("worker-2");
    std::fs::create_dir_all(&worker1).unwrap();
    std::fs::create_dir_all(&worker2).unwrap();
    let make_identity = |work: &str| TerminalIdentity {
        source_root: source.clone(),
        work_item_id: work.into(),
        attempt_id: "attempt-1".into(),
        task_id: "attempt-1-write-1".into(),
        role: "writer".into(),
        provider: "fixture".into(),
        artifact_dir: source
            .join(".fluent/work/artifacts")
            .join(work)
            .join("attempt-1/attempt-1-write-1"),
    };
    let resolver = fluent::content::ContentResolver::new(Some(&source));
    let home = std::env::var("HOME").unwrap();
    let profile = |identity: &TerminalIdentity, worker: &Path| {
        std::fs::create_dir_all(&identity.artifact_dir).unwrap();
        fluent::os::render_profile_common_only(
            &resolver,
            &home,
            &[worker.to_path_buf(), identity.artifact_dir.clone()],
            &[],
        )
        .unwrap()
    };
    let first_identity = make_identity("work-1");
    let second_identity = make_identity("work-2");
    let first_profile = profile(&first_identity, &worker1);
    let second_profile = profile(&second_identity, &worker2);
    fluent::os::preflight_profile_with(&first_profile, |path| {
        Command::new("/usr/bin/sandbox-exec")
            .arg("-f")
            .arg(path)
            .arg("/usr/bin/true")
            .output()
    })
    .unwrap();
    let environment = vec![
        ("PATH".into(), std::env::var_os("PATH").unwrap()),
        ("HOME".into(), home.into()),
        ("TERM".into(), "xterm-256color".into()),
    ];
    let mut first = TerminalSession::start_managed(
        first_identity,
        &first_profile,
        &worker1,
        "/bin/sh",
        &environment,
    )
    .unwrap();
    let mut second = TerminalSession::start_managed(
        second_identity.clone(),
        &second_profile,
        &worker2,
        "/bin/sh",
        &environment,
    )
    .unwrap();
    let first_socket = first.socket_path().unwrap().to_path_buf();
    let second_socket = second.socket_path().unwrap().to_path_buf();
    let allowed = worker1.join("allowed");
    let denied = worker2.join("denied");
    send(
        &first_socket,
        &format!("echo allowed > {}", allowed.display()),
    );
    wait_file(&allowed);
    send(
        &first_socket,
        &format!("echo denied > {}", denied.display()),
    );
    std::thread::sleep(Duration::from_millis(150));
    assert!(
        !denied.exists(),
        "first worker wrote in the second worker's root"
    );
    eprintln!(
        "terminal host confinement: allowed={} denied={}",
        allowed.exists(),
        denied.exists()
    );
    let foreign = worker1.join("foreign-socket-connected");
    send(
        &first_socket,
        &format!(
            "tmux -S {} list-sessions >/dev/null 2>&1 && touch {}",
            second_socket.display(),
            foreign.display()
        ),
    );
    std::thread::sleep(Duration::from_millis(150));
    assert!(
        !foreign.exists(),
        "first worker connected to the second socket"
    );
    eprintln!(
        "terminal host socket isolation: foreign_connected={}",
        foreign.exists()
    );
    let pid_file = worker1.join("descendant.pid");
    send(
        &first_socket,
        &format!("sleep 30 & echo $! > {}", pid_file.display()),
    );
    wait_file(&pid_file);
    let descendant: u32 = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let pane: u32 = String::from_utf8(
        Command::new("tmux")
            .arg("-S")
            .arg(&first_socket)
            .args(["list-panes", "-F", "#{pane_pid}"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .parse()
    .unwrap();
    let pane_group = group(pane);
    let descendant_group = group(descendant);
    eprintln!(
        "terminal host process scope: pane_pid={pane} pane_pgid={pane_group} descendant_pid={descendant} descendant_pgid={descendant_group}"
    );
    assert_ne!(
        descendant_group, pane_group,
        "fixture descendant did not enter another process group"
    );
    first.stop().unwrap();
    let descendant_state = Command::new("/bin/ps")
        .args(["-o", "stat=", "-p", &descendant.to_string()])
        .output()
        .unwrap();
    let state = String::from_utf8(descendant_state.stdout).unwrap();
    assert!(
        state.trim().is_empty() || state.trim().starts_with('Z'),
        "descendant remained live: {state}"
    );
    eprintln!(
        "terminal host settlement: descendant_state={:?}",
        state.trim()
    );
    assert!(
        second.is_live().unwrap(),
        "stopping one terminal disturbed another"
    );
    second.stop().unwrap();

    unsafe {
        std::env::set_var("FLUENT_TEST_TERMINAL_FAIL_AFTER_SERVER", "1");
    }
    let failure = TerminalSession::start_managed(
        second_identity.clone(),
        &second_profile,
        &worker2,
        "/bin/sh",
        &environment,
    );
    unsafe {
        std::env::remove_var("FLUENT_TEST_TERMINAL_FAIL_AFTER_SERVER");
    }
    assert!(
        failure.is_err(),
        "setup fault unexpectedly produced a terminal"
    );
    let resource = source
        .join(".fluent/work/terminal-sessions/work-2/attempt-1/attempt-1-write-1/resource.json");
    assert!(
        !resource.exists(),
        "failed setup left a terminal resource receipt"
    );
    eprintln!(
        "terminal host partial setup: resource_present={}",
        resource.exists()
    );

    let barrier = source.join("setup-barrier");
    unsafe {
        std::env::set_var("FLUENT_TEST_TERMINAL_SETUP_BARRIER", &barrier);
    }
    std::thread::scope(|scope| {
        let record = second_identity.artifact_dir.join("execution.json");
        let launch = scope.spawn(|| {
            TerminalSession::start_managed(
                second_identity,
                &second_profile,
                &worker2,
                "/bin/sh",
                &environment,
            )
        });
        wait_file(&barrier.with_extension("ready"));
        let outcome = fluent::execution::cancel(&record).unwrap();
        eprintln!("terminal host cancellation during setup: {outcome:?}");
        std::fs::write(&barrier, b"release").unwrap();
        assert!(launch.join().unwrap().is_err());
    });
    unsafe {
        std::env::remove_var("FLUENT_TEST_TERMINAL_SETUP_BARRIER");
    }
    assert!(
        !resource.exists(),
        "canceled setup left a terminal resource receipt"
    );
}
