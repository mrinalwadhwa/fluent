#![cfg(feature = "test-support")]

use fluent::terminal_sessions::{ControlOutcome, TerminalIdentity, TerminalSession};

#[cfg(target_os = "macos")]
#[test]
fn terminal_policy_membership_requires_distinct_valid_grants() {
    use fluent::terminal_sessions::policy_grants_owner_only;

    assert!(policy_grants_owner_only(0, 1));
    for (own, peer) in [(0, 0), (1, 1), (1, 0), (-1, 1), (0, -1), (-1, -1)] {
        assert!(!policy_grants_owner_only(own, peer), "{own}, {peer}");
    }
}

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
    if std::env::var_os("FLUENT_TEST_REAL_TERMINAL_HOST").is_some() {
        host_managed_terminal_acceptance();
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let identity = TerminalIdentity::fixture(root.path(), "work-1", "attempt-1", "task-1");
    let mut session = TerminalSession::start_fixture(identity, "sleep 30").unwrap();
    let descriptor = session.descriptor().clone();
    session.observe(&descriptor).unwrap().detach();
    assert!(session.is_live().unwrap());
    session.stop().unwrap();
}

#[test]
fn completed_terminal_settles_without_canceling_its_owner() {
    let root = tempfile::tempdir().unwrap();
    let identity = TerminalIdentity::fixture(root.path(), "work-1", "attempt-1", "task-1");
    let record_path = identity.artifact_dir.join("execution.json");
    let mut session = TerminalSession::start_fixture(identity, "sleep 30").unwrap();
    session.finish().unwrap();
    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(record_path).unwrap()).unwrap();
    assert_eq!(record["state"], "settled");
    assert!(!session.is_live().unwrap());
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
    if std::env::var_os("FLUENT_TEST_REAL_TERMINAL_HOST").is_some() {
        host_managed_terminal_acceptance();
        return;
    }
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

#[cfg(target_os = "macos")]
#[test]
fn attached_clients_keep_control_occupied_until_writable_client_detaches() {
    if std::env::var_os("FLUENT_TEST_REAL_TERMINAL_HOST").is_none() {
        return;
    }
    use std::io::Write;
    use std::path::Path;
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    let root = tempfile::tempdir().unwrap();
    let identity = TerminalIdentity::fixture(root.path(), "work-1", "attempt-1", "task-1");
    let mut session = TerminalSession::start_tmux_fixture(identity).unwrap();
    let socket = session.socket_path().unwrap().to_path_buf();
    let descriptor = session.descriptor().clone();
    let attach = || {
        let output = tempfile::NamedTempFile::new().unwrap();
        let errors = tempfile::NamedTempFile::new().unwrap();
        let child = Command::new("/usr/bin/script")
            .arg("-q")
            .arg("/dev/null")
            .arg(assert_cmd::cargo::cargo_bin("fluent"))
            .args(["terminal", "attach", &descriptor.session_id, "--source"])
            .arg(root.path())
            .arg("--control")
            .env("TERM", "xterm-256color")
            .env_remove("TMUX")
            .stdin(Stdio::piped())
            .stdout(Stdio::from(output.reopen().unwrap()))
            .stderr(Stdio::from(errors.reopen().unwrap()))
            .spawn()
            .unwrap();
        (child, output, errors)
    };
    let clients = || -> Vec<String> {
        let output = Command::new("tmux")
            .arg("-S")
            .arg(&socket)
            .args(["list-clients", "-F", "#{client_tty}"])
            .output()
            .unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    };
    fn wait_for(mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !condition() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(
            condition(),
            "timed out waiting for terminal attachment state"
        );
    }
    let send = |child: &mut Child, marker: &Path| {
        writeln!(
            child.stdin.as_mut().unwrap(),
            "echo reached > {}",
            marker.display()
        )
        .unwrap();
        child.stdin.as_mut().unwrap().flush().unwrap();
    };

    let (mut first, _first_output, _first_errors) = attach();
    wait_for(|| clients().len() == 1);
    let first_tty = clients().pop().unwrap();
    let first_marker = root.path().join("first-marker");
    send(&mut first, &first_marker);
    wait_for(|| first_marker.exists());

    let (mut second, second_output, _second_errors) = attach();
    wait_for(|| clients().len() == 2);
    wait_for(|| {
        std::fs::read(second_output.path())
            .unwrap()
            .windows(b"Control is occupied".len())
            .any(|part| part == b"Control is occupied")
    });
    let ignored_marker = root.path().join("observer-marker");
    send(&mut second, &ignored_marker);
    std::thread::sleep(Duration::from_millis(250));
    assert!(
        !ignored_marker.exists(),
        "observer input reached the worker"
    );

    assert!(
        Command::new("tmux")
            .arg("-S")
            .arg(&socket)
            .args(["detach-client", "-t", &first_tty])
            .status()
            .unwrap()
            .success()
    );
    wait_for(|| first.try_wait().unwrap().is_some());
    let (mut third, third_output, _third_errors) = attach();
    wait_for(|| clients().len() == 2);
    assert!(
        !std::fs::read(third_output.path())
            .unwrap()
            .windows(b"Control is occupied".len())
            .any(|part| part == b"Control is occupied")
    );
    let third_marker = root.path().join("third-marker");
    send(&mut third, &third_marker);
    wait_for(|| third_marker.exists());
    assert!(!ignored_marker.exists());
    assert!(session.is_live().unwrap());
    Command::new("tmux")
        .arg("-S")
        .arg(&socket)
        .args(["detach-client", "-s", "fluent"])
        .status()
        .unwrap();
    let _ = second.wait();
    let _ = third.wait();
    session.stop().unwrap();
    std::fs::remove_dir_all(socket.parent().unwrap()).unwrap();
}

#[test]
fn unavailable_terminal_has_specific_reason() {
    use fluent::terminal_sessions::availability;
    use fluent::work_model::{TaskKind, TaskOutput, TaskStatus, WorkItem, WorkModelStore};
    let root = tempfile::tempdir().unwrap();
    let store = WorkModelStore::new(root.path());
    for (work, kind, status, has_execution, reason) in [
        (
            "legacy",
            TaskKind::Write,
            TaskStatus::Planned,
            false,
            "legacy",
        ),
        (
            "headless",
            TaskKind::Write,
            TaskStatus::Executing,
            true,
            "headless",
        ),
        (
            "completed",
            TaskKind::Write,
            TaskStatus::Complete,
            true,
            "completed",
        ),
        (
            "tester",
            TaskKind::Tester,
            TaskStatus::Executing,
            true,
            "Tester",
        ),
    ] {
        let mut item = WorkItem::planned(work, "Terminal availability");
        item.add_initial_attempt("attempt-1").unwrap();
        let task = &mut item.attempts[0].tasks[0];
        task.kind = kind;
        task.status = status;
        if task.status == TaskStatus::Complete {
            task.output = Some(TaskOutput {
                workspace_id: "fixture-workspace".into(),
                workspace_path: "fixture-workspace".into(),
                source_branch: "fixture-branch".into(),
                base_commit: None,
                commit: "0123456789abcdef0123456789abcdef01234567".into(),
                no_change: None,
                learner_canonicalization: None,
            });
        }
        store.create_work_item(&item).unwrap();
        let artifact = root.path().join(format!(
            ".fluent/work/artifacts/{work}/attempt-1/attempt-1-write-1"
        ));
        if has_execution {
            std::fs::create_dir_all(&artifact).unwrap();
            std::fs::write(artifact.join("execution.json"), b"{}").unwrap();
        }
        let selected = availability(root.path(), work, "attempt-1", "attempt-1-write-1").unwrap();
        assert!(selected.descriptor.is_none());
        assert!(
            selected.unavailable_reason.unwrap().contains(reason),
            "{work}"
        );
        let output = assert_cmd::Command::cargo_bin("fluent")
            .unwrap()
            .args([
                "terminal",
                "explain",
                "--source",
                root.path().to_str().unwrap(),
                work,
                "attempt-1",
                "attempt-1-write-1",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains(reason));
    }
}

#[test]
fn settlement_covers_terminal_process_scope() {
    if std::env::var_os("FLUENT_TEST_REAL_TERMINAL_HOST").is_some() {
        host_managed_terminal_acceptance();
        return;
    }
    let table = "10 1 10\n20 10 20\n21 20 21\n22 21 22\n30 1 30\n";
    let owned = fluent::terminal_sessions::descendants_from_ps(table, &[20]).unwrap();
    assert_eq!(owned, vec![20, 21, 22]);
    assert!(!owned.contains(&30));
}

#[test]
fn terminal_owner_inventory_parses_open_handle_pids() {
    let table = "p20\nf9\np41\nf9\n";
    assert_eq!(
        fluent::terminal_sessions::owner_members_from_lsof(table).unwrap(),
        vec![20, 41]
    );
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
    if std::env::var_os("FLUENT_TEST_REAL_TERMINAL_HOST").is_some() {
        host_managed_terminal_acceptance();
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let first = TerminalIdentity::fixture(root.path(), "work-1", "attempt-1", "task-1");
    let second = TerminalIdentity::fixture(root.path(), "work-1", "attempt-1", "task-2");
    let mut live = TerminalSession::start_fixture(first, "sleep 30").unwrap();
    assert!(TerminalSession::start_fixture_failed_setup(second).is_err());
    assert!(live.is_live().unwrap());
    live.stop().unwrap();
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires a real tmux pane under the macOS worker sandbox"]
fn managed_terminal_pane_starts_in_assigned_workspace() {
    use fluent::work_model::{WorkItem, WorkModelStore};
    use std::process::Command;
    use std::time::{Duration, Instant};

    let root = tempfile::tempdir_in("/private/tmp").unwrap();
    let source = root.path().canonicalize().unwrap();
    let workspace = source.join("assigned-workspace");
    let home = workspace.join("private-home");
    std::fs::create_dir_all(&home).unwrap();
    assert_ne!(
        std::env::current_dir().unwrap().canonicalize().unwrap(),
        workspace
    );

    let mut item = WorkItem::planned("work-1", "Terminal cwd fixture");
    item.add_initial_attempt("attempt-1").unwrap();
    WorkModelStore::new(&source)
        .create_work_item(&item)
        .unwrap();
    let identity = TerminalIdentity {
        source_root: source.clone(),
        work_item_id: "work-1".into(),
        attempt_id: "attempt-1".into(),
        task_id: "attempt-1-write-1".into(),
        role: "writer".into(),
        provider: "fixture".into(),
        artifact_dir: source.join(".fluent/work/artifacts/work-1/attempt-1/attempt-1-write-1"),
    };
    std::fs::create_dir_all(&identity.artifact_dir).unwrap();
    let resolver = fluent::content::ContentResolver::new(Some(&source));
    let profile = fluent::os::render_profile_common_only(
        &resolver,
        home.to_str().unwrap(),
        &[workspace.clone(), identity.artifact_dir.clone()],
        &[],
    )
    .unwrap();
    let environment = vec![
        ("PATH".into(), std::env::var_os("PATH").unwrap()),
        ("HOME".into(), home.into_os_string()),
        ("TERM".into(), "xterm-256color".into()),
    ];
    let mut session =
        TerminalSession::start_managed(identity, &profile, &workspace, "/bin/sh", &environment)
            .unwrap();
    let actual_cwd = workspace.join("pane-cwd.txt");
    let socket = session.socket_path().unwrap();
    let command = format!("/bin/pwd > {}", actual_cwd.display());
    let probe = (|| -> anyhow::Result<String> {
        let send = Command::new("tmux")
            .arg("-S")
            .arg(socket)
            .args(["send-keys", "-l", &command])
            .output()?;
        anyhow::ensure!(
            send.status.success(),
            "sending pwd command: {}",
            String::from_utf8_lossy(&send.stderr)
        );
        let enter = Command::new("tmux")
            .arg("-S")
            .arg(socket)
            .args(["send-keys", "Enter"])
            .output()?;
        anyhow::ensure!(
            enter.status.success(),
            "submitting pwd command: {}",
            String::from_utf8_lossy(&enter.stderr)
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while !actual_cwd.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
        }
        anyhow::ensure!(actual_cwd.exists(), "pane did not write physical cwd");
        Ok(std::fs::read_to_string(&actual_cwd)?)
    })();
    session.stop().unwrap();
    let observed = probe.unwrap();
    assert_eq!(observed.trim(), workspace.to_str().unwrap());
}

#[test]
fn host_managed_terminal_acceptance() {
    if std::env::var_os("FLUENT_TEST_REAL_TERMINAL_HOST").is_none() {
        return;
    }
    use fluent::work_model::{WorkItem, WorkModelStore};
    use std::path::Path;
    use std::process::{Command, Stdio};
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
    let first = TerminalSession::start_managed(
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
    let first_record =
        source.join(".fluent/work/artifacts/work-1/attempt-1/attempt-1-write-1/execution.json");
    let original: fluent::execution::ExecutionRecord =
        serde_json::from_slice(&std::fs::read(&first_record).unwrap()).unwrap();
    let attach_and_disconnect = || {
        // script gives the CLI a real controlling PTY, as a dashboard or
        // terminal client would. Keep its input pipe open until tmux detaches.
        let output = tempfile::NamedTempFile::new_in(&source).unwrap();
        let errors = tempfile::NamedTempFile::new_in(&source).unwrap();
        let mut client = Command::new("/usr/bin/script")
            .arg("-q")
            .arg("/dev/null")
            .arg(assert_cmd::cargo::cargo_bin("fluent"))
            .args([
                "terminal",
                "attach",
                "--source",
                source.to_str().unwrap(),
                &first.descriptor().session_id,
            ])
            .env("TERM", "xterm-256color")
            .env_remove("TMUX")
            .stdin(Stdio::piped())
            .stdout(Stdio::from(output.reopen().unwrap()))
            .stderr(Stdio::from(errors.reopen().unwrap()))
            .spawn()
            .unwrap();
        let diagnostics = |status| {
            format!(
                "status={status:?}, output={}, errors={}",
                String::from_utf8_lossy(&std::fs::read(output.path()).unwrap()),
                String::from_utf8_lossy(&std::fs::read(errors.path()).unwrap())
            )
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let clients = Command::new("tmux")
                .arg("-S")
                .arg(&first_socket)
                .args(["list-clients", "-F", "#{client_pid}"])
                .output()
                .unwrap();
            if clients.status.success() && !clients.stdout.is_empty() {
                break;
            }
            if let Some(status) = client.try_wait().unwrap() {
                panic!(
                    "attachment exited before connecting: {}",
                    diagnostics(Some(status))
                );
            }
            if Instant::now() >= deadline {
                client.kill().unwrap();
                let status = client.wait().unwrap();
                panic!("attachment did not connect: {}", diagnostics(Some(status)));
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(
            Command::new("tmux")
                .arg("-S")
                .arg(&first_socket)
                .args(["detach-client", "-s", "fluent"])
                .status()
                .unwrap()
                .success()
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while client.try_wait().unwrap().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
        }
        if client.try_wait().unwrap().is_none() {
            client.kill().unwrap();
            let status = client.wait().unwrap();
            panic!(
                "attachment did not disconnect: {}",
                diagnostics(Some(status))
            );
        }
        assert!(first.is_live().unwrap(), "attachment canceled its worker");
        let current: fluent::execution::ExecutionRecord =
            serde_json::from_slice(&std::fs::read(&first_record).unwrap()).unwrap();
        assert_eq!(current.launch_generation, original.launch_generation);
        assert_eq!(current.leader_pid, original.leader_pid);
    };
    attach_and_disconnect();
    attach_and_disconnect();
    let marker = first_socket
        .parent()
        .unwrap()
        .file_name()
        .unwrap()
        .to_string_lossy();
    let mut unrelated = Command::new("/usr/bin/perl")
        .args(["-e", "sleep 30", &format!("FLUENT_TERMINAL_OWNER={marker}")])
        .spawn()
        .unwrap();
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
    let orphan_file = worker1.join("orphan.pid");
    send(
        &first_socket,
        &format!(
            "/bin/sh -c 'sleep 30 & echo $! > {}'",
            orphan_file.display()
        ),
    );
    wait_file(&orphan_file);
    let orphan: u32 = std::fs::read_to_string(&orphan_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let orphan_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let parent = String::from_utf8(
            Command::new("/bin/ps")
                .args(["-o", "ppid=", "-p", &orphan.to_string()])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        if parent.trim() == "1" {
            break;
        }
        assert!(
            Instant::now() < orphan_deadline,
            "orphan parent did not exit"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    let new_session_file = worker1.join("new-session.pid");
    send(
        &first_socket,
        &format!(
            "/usr/bin/perl -MPOSIX -e 'if (fork()) {{ exit 0 }} POSIX::setsid(); open my $fh, \">\", $ARGV[0] or die $!; print $fh \"$$\\n\"; close $fh; %ENV = (); exec \"/bin/sleep\", \"30\"' {}",
            new_session_file.display()
        ),
    );
    wait_file(&new_session_file);
    let new_session: u32 = std::fs::read_to_string(&new_session_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_ne!(
        group(new_session),
        pane_group,
        "fixture child did not enter a new session"
    );
    let new_session_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let parent = String::from_utf8(
            Command::new("/bin/ps")
                .args(["-o", "ppid=", "-p", &new_session.to_string()])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        if parent.trim() == "1" {
            break;
        }
        assert!(
            Instant::now() < new_session_deadline,
            "new-session child was not reparented"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    let closed_handle_file = worker1.join("closed-handle.pid");
    send(
        &first_socket,
        &format!(
            "/usr/bin/perl -MPOSIX -e 'if (fork()) {{ exit 0 }} POSIX::setsid(); POSIX::close(9); open my $fh, \">\", $ARGV[0] or die $!; print $fh \"$$\\n\"; close $fh; %ENV = (); exec \"/bin/sleep\", \"30\"' {}",
            closed_handle_file.display()
        ),
    );
    wait_file(&closed_handle_file);
    let closed_handle: u32 = std::fs::read_to_string(&closed_handle_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_ne!(group(closed_handle), pane_group);
    let closed_handle_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let parent = String::from_utf8(
            Command::new("/bin/ps")
                .args(["-o", "ppid=", "-p", &closed_handle.to_string()])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        if parent.trim() == "1" {
            break;
        }
        assert!(
            Instant::now() < closed_handle_deadline,
            "closed-handle child was not reparented"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    let open_handles = Command::new("/usr/sbin/lsof")
        .args(["-F", "p", "--"])
        .arg(first_socket.with_file_name("owner"))
        .output()
        .unwrap();
    assert!(
        !String::from_utf8(open_handles.stdout)
            .unwrap()
            .lines()
            .any(|line| line == format!("p{closed_handle}")),
        "fixture child retained the terminal owner handle"
    );
    #[cfg(target_os = "macos")]
    {
        let members =
            fluent::terminal_sessions::policy_members_for_test(&first_socket, &[pane]).unwrap();
        assert!(
            members.contains(&closed_handle),
            "closed-handle child lost its policy grant"
        );
        assert!(
            !members.contains(&unrelated.id()),
            "unrestricted host process was adopted"
        );
        assert!(
            !members.contains(&std::process::id()),
            "unrestricted test process was adopted"
        );
        let second_pane: u32 = String::from_utf8(
            Command::new("tmux")
                .arg("-S")
                .arg(&second_socket)
                .args(["list-panes", "-F", "#{pane_pid}"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .parse()
        .unwrap();
        assert!(
            !members.contains(&second_pane),
            "another invocation was adopted"
        );
    }
    let canceled = fluent::execution::cancel(&first_record).unwrap();
    eprintln!("terminal host external cancellation: {canceled:?}");
    let descendant_state = Command::new("/bin/ps")
        .args(["-o", "stat=", "-p", &descendant.to_string()])
        .output()
        .unwrap();
    let state = String::from_utf8(descendant_state.stdout).unwrap();
    assert!(
        state.trim().is_empty() || state.trim().starts_with('Z'),
        "descendant remained live: {state}"
    );
    let orphan_state = String::from_utf8(
        Command::new("/bin/ps")
            .args(["-o", "stat=", "-p", &orphan.to_string()])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert!(
        orphan_state.trim().is_empty() || orphan_state.trim().starts_with('Z'),
        "orphan descendant remained live: {orphan_state}"
    );
    let new_session_state = String::from_utf8(
        Command::new("/bin/ps")
            .args(["-o", "stat=", "-p", &new_session.to_string()])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert!(
        new_session_state.trim().is_empty() || new_session_state.trim().starts_with('Z'),
        "reparented new-session child remained live: {new_session_state}"
    );
    let closed_handle_state = String::from_utf8(
        Command::new("/bin/ps")
            .args(["-o", "stat=", "-p", &closed_handle.to_string()])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert!(
        closed_handle_state.trim().is_empty() || closed_handle_state.trim().starts_with('Z'),
        "closed-handle new-session child remained live: {closed_handle_state}"
    );
    assert!(
        unrelated.try_wait().unwrap().is_none(),
        "unrelated process with owner marker in its argument was signaled"
    );
    unrelated.kill().unwrap();
    unrelated.wait().unwrap();
    let first_resource = source
        .join(".fluent/work/terminal-sessions/work-1/attempt-1/attempt-1-write-1/resource.json");
    assert!(
        !first_resource.exists(),
        "external cancellation left its resource receipt"
    );
    let mut next = TerminalSession::start_managed(
        make_identity("work-1"),
        &first_profile,
        &worker1,
        "/bin/sh",
        &environment,
    )
    .unwrap();
    assert_ne!(
        next.descriptor().execution_generation,
        first.descriptor().execution_generation
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
    assert!(
        format!("{:#}", failure.as_ref().err().unwrap())
            .contains("injected terminal failure after server startup")
    );
    assert!(
        next.is_live().unwrap(),
        "failed setup disturbed the peer terminal"
    );
    let resource = source
        .join(".fluent/work/terminal-sessions/work-2/attempt-1/attempt-1-write-1/resource.json");
    assert!(
        !resource.exists(),
        "failed setup left a terminal resource receipt: {:#?}",
        failure.err()
    );
    eprintln!(
        "terminal host partial setup: resource_present={}",
        resource.exists()
    );
    next.stop().unwrap();

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
