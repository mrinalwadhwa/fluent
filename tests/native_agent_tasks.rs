use fluent::native_agent::{CodexRelayGate, Completion, NativeAdmission, PiSettlementGate};
use fluent::work_model::{Attempt, CoderMapping, TerminalMode, WorkItem, reopen_attempt};
use serde_json::json;

#[test]
fn codex_relay_keeps_assigned_thread() {
    let mut gate = CodexRelayGate::new("generation-2", "assigned-thread");
    assert!(
        gate.admit_request(
            &json!({"id": 1, "method": "thread/resume", "params": {"threadId": "assigned-thread"}})
        )
        .is_ok()
    );
    assert!(
        gate.admit_request(
            &json!({"id": 2, "method": "thread/resume", "params": {"threadId": "other-thread"}})
        )
        .is_err()
    );
    assert!(
        gate.admit_request(&json!({"id": 3, "method": "thread/start", "params": {}}))
            .is_err()
    );
    assert_eq!(
        gate.admit_request(&json!({"id": 4, "method": "turn/start", "params": {"threadId": "assigned-thread", "input": []}})).unwrap(),
        Some(1)
    );
    assert!(gate
        .admit_request(&json!({"id": 5, "method": "turn/start", "params": {"threadId": "other-thread", "input": []}}))
        .is_err());
}

#[test]
fn codex_relay_rejects_old_turn_completion() {
    let mut gate = CodexRelayGate::new("generation-2", "assigned-thread");
    gate.admit_request(&json!({"id": 1, "method": "turn/start", "params": {"threadId": "assigned-thread", "input": []}})).unwrap();
    gate.observe_event_for_generation("generation-2", &json!({"method": "turn/started", "params": {"threadId": "assigned-thread", "turn": {"id": "turn-1"}}})).unwrap();
    assert_eq!(gate.observe_event_for_generation("generation-2", &json!({"method": "turn/completed", "params": {"threadId": "assigned-thread", "turn": {"id": "turn-1", "status": "completed"}}})).unwrap(), Completion::Accepted { generation: "generation-2".into(), turn_id: "turn-1".into(), input_sequence: 1 });
    gate.admit_request(&json!({"id": 2, "method": "turn/start", "params": {"threadId": "assigned-thread", "input": []}})).unwrap();
    gate.observe_event_for_generation("generation-2", &json!({"method": "turn/started", "params": {"threadId": "assigned-thread", "turn": {"id": "turn-2"}}})).unwrap();
    assert_eq!(gate.observe_event_for_generation("generation-2", &json!({"method": "turn/completed", "params": {"threadId": "assigned-thread", "turn": {"id": "turn-1", "status": "completed"}}})).unwrap(), Completion::Ignored);
    assert_eq!(gate.observe_event_for_generation("generation-2", &json!({"method": "turn/completed", "params": {"threadId": "other-thread", "turn": {"id": "turn-2", "status": "completed"}}})).unwrap(), Completion::Ignored);
    assert_eq!(gate.observe_event_for_generation("generation-2", &json!({"method": "turn/completed", "params": {"threadId": "assigned-thread", "turn": {"id": "turn-2", "status": "interrupted"}}})).unwrap(), Completion::Ignored);
}

#[test]
fn old_turn_cannot_complete_current_invocation() {
    let mut gate = CodexRelayGate::new("generation-2", "assigned-thread");
    gate.admit_request(&json!({"id": 1, "method": "turn/start", "params": {"threadId": "assigned-thread", "input": []}})).unwrap();
    let started = json!({"method": "turn/started", "params": {"threadId": "assigned-thread", "turn": {"id": "turn-1"}}});
    let completed = json!({"method": "turn/completed", "params": {"threadId": "assigned-thread", "turn": {"id": "turn-1", "status": "completed"}}});
    assert_eq!(
        gate.observe_event_for_generation("generation-1", &started)
            .unwrap(),
        Completion::Ignored
    );
    assert_eq!(
        gate.observe_event_for_generation("generation-1", &completed)
            .unwrap(),
        Completion::Ignored
    );
    assert_eq!(
        gate.observe_event_for_generation("generation-2", &started)
            .unwrap(),
        Completion::Ignored
    );
    assert_eq!(
        gate.observe_event_for_generation("generation-1", &completed)
            .unwrap(),
        Completion::Ignored
    );
    assert_eq!(
        gate.observe_event_for_generation("generation-2", &completed)
            .unwrap(),
        Completion::Accepted {
            generation: "generation-2".into(),
            turn_id: "turn-1".into(),
            input_sequence: 1,
        }
    );
    assert_eq!(
        gate.observe_event_for_generation("generation-2", &completed)
            .unwrap(),
        Completion::Ignored
    );
}

#[test]
fn settlement_closes_input_before_advancement() {
    use std::sync::{Arc, Barrier};

    let admission = Arc::new(NativeAdmission::new());
    assert_eq!(admission.admit().unwrap(), 1);
    let barrier = Arc::new(Barrier::new(3));
    let admitted = {
        let admission = Arc::clone(&admission);
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            barrier.wait();
            admission.admit().ok()
        })
    };
    let cutoff = {
        let admission = Arc::clone(&admission);
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            barrier.wait();
            admission.begin_settlement().unwrap()
        })
    };
    barrier.wait();
    let admitted = admitted.join().unwrap();
    let cutoff = cutoff.join().unwrap();
    assert_eq!(cutoff, admitted.unwrap_or(1));
    assert!(admission.admit().is_err());
    assert!(admission.begin_settlement().is_err());

    let shared = Arc::new(NativeAdmission::new());
    let mut relay =
        CodexRelayGate::with_admission("generation-3", "assigned-thread", Arc::clone(&shared));
    assert_eq!(relay.admit_request(&json!({"id": 1, "method": "turn/start", "params": {"threadId": "assigned-thread", "input": []}})).unwrap(), Some(1));
    assert_eq!(shared.begin_settlement().unwrap(), 1);
    assert!(relay.admit_request(&json!({"id": 2, "method": "turn/steer", "params": {"threadId": "assigned-thread", "input": []}})).is_err());
}

#[test]
fn pi_native_waits_for_queued_input_before_settlement() {
    let mut gate = PiSettlementGate::new("generation-2", "session-1");
    assert_eq!(gate.admit_input("generation-2", "session-1").unwrap(), 1);
    assert_eq!(gate.admit_input("generation-2", "session-1").unwrap(), 2);

    // Pi may emit agent_end for an earlier turn while queued steering or a
    // follow-up still has work to do. Even agent_settled cannot close an input
    // whose handler returned before Pi placed it in the session.
    assert_eq!(
        gate.observe_event("generation-2", "session-1", &json!({"type": "agent_end"}))
            .unwrap(),
        Completion::Ignored
    );
    assert_eq!(
        gate.observe_event(
            "generation-2",
            "session-1",
            &json!({"type": "agent_settled", "aborted": false})
        )
        .unwrap(),
        Completion::Ignored
    );

    gate.observe_input_entry("generation-2", "session-1", 1, "entry-1")
        .unwrap();
    assert_eq!(
        gate.observe_event(
            "generation-2",
            "session-1",
            &json!({"type": "agent_settled", "aborted": false})
        )
        .unwrap(),
        Completion::Ignored
    );
    gate.observe_input_entry("generation-2", "session-1", 2, "entry-2")
        .unwrap();
    assert_eq!(
        gate.observe_event(
            "generation-1",
            "session-1",
            &json!({"type": "agent_settled", "aborted": false})
        )
        .unwrap(),
        Completion::Ignored
    );
    assert_eq!(
        gate.observe_event(
            "generation-2",
            "other-session",
            &json!({"type": "agent_settled", "aborted": false})
        )
        .unwrap(),
        Completion::Ignored
    );
    assert_eq!(
        gate.observe_event(
            "generation-2",
            "session-1",
            &json!({"type": "agent_settled", "aborted": false})
        )
        .unwrap(),
        Completion::Accepted {
            generation: "generation-2".into(),
            turn_id: "entry-2".into(),
            input_sequence: 2,
        }
    );
    assert!(gate.admit_input("generation-2", "session-1").is_err());
    assert_eq!(
        gate.observe_event(
            "generation-2",
            "session-1",
            &json!({"type": "agent_settled", "aborted": false})
        )
        .unwrap(),
        Completion::Ignored
    );

    let mut aborted = PiSettlementGate::new("generation-3", "session-2");
    aborted.admit_input("generation-3", "session-2").unwrap();
    aborted
        .observe_input_entry("generation-3", "session-2", 1, "entry-1")
        .unwrap();
    assert!(
        aborted
            .observe_event(
                "generation-3",
                "session-2",
                &json!({"type": "agent_settled", "aborted": true})
            )
            .is_err()
    );
    assert!(aborted.admit_input("generation-3", "session-2").is_err());
}

#[cfg(feature = "test-support")]
#[test]
fn attached_operator_does_not_hold_workflow_progression() {
    use fluent::terminal_sessions::{ControlOutcome, TerminalIdentity, TerminalSession};
    use std::sync::mpsc;
    use std::time::Duration;

    let root = tempfile::tempdir().unwrap();
    let identity = TerminalIdentity::fixture(root.path(), "work-1", "attempt-1", "task-1");
    let session = TerminalSession::start_fixture(identity, "sleep 30").unwrap();
    let mut observer = session.observe(session.descriptor()).unwrap();
    assert_eq!(
        observer.acquire_control().unwrap(),
        ControlOutcome::Acquired
    );
    let (sent, received) = mpsc::channel();
    let stop = std::thread::spawn(move || {
        let mut session = session;
        sent.send(session.stop()).unwrap();
    });

    received
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    stop.join().unwrap();
    assert!(observer.acquire_control().is_err());

    let next = TerminalIdentity::fixture(root.path(), "work-1", "attempt-1", "task-2");
    let mut next_session = TerminalSession::start_fixture(next, "sleep 30").unwrap();
    next_session.stop().unwrap();
}

#[test]
fn attempt_retains_native_mode_across_resume() {
    use clap::Parser;
    use fluent::cli::{AttemptCommands, Cli, Commands};

    let cli = Cli::try_parse_from([
        "fluent",
        "attempt",
        "create",
        "work-1",
        "--terminal-mode",
        "native",
    ])
    .unwrap();
    assert!(matches!(cli.command,
        Some(Commands::Attempt { command: AttemptCommands::Create { terminal_mode: Some(mode), .. } })
        if mode == "native"));

    let mut attempt = Attempt {
        terminal_mode: TerminalMode::Native,
        ..Default::default()
    };
    let saved = serde_json::to_string(&attempt).unwrap();
    attempt = serde_json::from_str(&saved).unwrap();
    reopen_attempt(&mut attempt);
    assert_eq!(attempt.terminal_mode, TerminalMode::Native);

    let mut legacy: serde_json::Value = serde_json::from_str(&saved).unwrap();
    legacy.as_object_mut().unwrap().remove("terminal_mode");
    let legacy: Attempt = serde_json::from_value(legacy).unwrap();
    assert_eq!(legacy.terminal_mode, TerminalMode::Headless);

    let mut item = WorkItem {
        id: "work-1".into(),
        title: "Native mode fixture".into(),
        ..Default::default()
    };
    item.add_initial_attempt_with_settings(
        "attempt-1",
        CoderMapping::default(),
        TerminalMode::Native,
    )
    .unwrap();
    assert_eq!(item.attempts[0].terminal_mode, TerminalMode::Native);
    let stored_project = tempfile::tempdir().unwrap();
    let store = fluent::work_model::WorkModelStore::new(stored_project.path());
    store.create_work_item(&item).unwrap();
    let attempt_record: serde_json::Value = serde_json::from_slice(
        &std::fs::read(
            stored_project
                .path()
                .join(".fluent/work/attempts/work-1/attempt-1.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(attempt_record["schema_version"], 13);
    assert_eq!(attempt_record["terminal_mode"], "native");
    let mut loaded = store.read_work_item_snapshot("work-1").unwrap();
    reopen_attempt(&mut loaded.attempts[0]);
    assert_eq!(loaded.attempts[0].terminal_mode, TerminalMode::Native);

    let project = tempfile::tempdir().unwrap();
    std::fs::create_dir(project.path().join(".fluent")).unwrap();
    std::fs::write(
        project.path().join(".fluent/config.yaml"),
        "terminal:\n  mode: native\n",
    )
    .unwrap();
    assert_eq!(
        fluent::config::attempt_terminal_mode(project.path(), None).unwrap(),
        TerminalMode::Native
    );
    assert_eq!(
        fluent::config::attempt_terminal_mode(project.path(), Some("headless")).unwrap(),
        TerminalMode::Headless
    );
}

#[test]
fn native_task_preserves_private_provider_boundary() {
    use fluent::coder::{CoderSession, CodexCode};
    use std::ffi::OsStr;

    let project = tempfile::tempdir().unwrap();
    let private_home = tempfile::tempdir().unwrap();
    let coder = CodexCode {
        sandbox_profile: Some("(version 1) (allow default)".into()),
        trusted_sandbox: false,
        model_override: Some("gpt-assigned".into()),
        effort: Some("high".into()),
        executable: "/private/task/codex".into(),
    };
    let command = coder
        .native_task_command(
            project.path(),
            private_home.path(),
            CoderSession::Ephemeral,
            "assigned Task prompt",
        )
        .unwrap();
    let args: Vec<_> = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert_eq!(command.get_program(), OsStr::new("sandbox-exec"));
    assert!(
        args.windows(2)
            .any(|args| args == ["-f", "(version 1) (allow default)"])
    );
    assert!(args.contains(&"/private/task/codex".to_string()));
    assert!(args.windows(2).any(|args| args == ["--disable", "hooks"]));
    assert!(
        args.windows(2)
            .any(|args| args == ["--model", "gpt-assigned"])
    );
    assert!(
        args.windows(2)
            .any(|args| args == ["-c", "model_reasoning_effort=high"])
    );
    assert_eq!(args.last().unwrap(), "assigned Task prompt");
    assert!(
        !args
            .iter()
            .any(|arg| arg == "exec" || arg == "--ignore-user-config")
    );
    assert!(
        command
            .get_envs()
            .any(|(name, value)| name == OsStr::new("CODEX_HOME")
                && value == Some(private_home.path().as_os_str()))
    );

    let resumed = coder
        .native_task_command(
            project.path(),
            private_home.path(),
            CoderSession::Resume("assigned-session"),
            "continue Task",
        )
        .unwrap();
    let resumed_args: Vec<_> = resumed
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert!(
        resumed_args
            .windows(2)
            .any(|args| args == ["resume", "assigned-session"])
    );
    assert!(!resumed_args.contains(&"--last".to_string()));

    assert!(
        coder
            .native_task_command(
                project.path(),
                &project.path().join("absent-home"),
                CoderSession::FreshPersistent,
                "assigned Task prompt",
            )
            .is_err()
    );
    let unspecified_model = CodexCode {
        model_override: None,
        ..coder
    };
    assert!(
        unspecified_model
            .native_task_command(
                project.path(),
                private_home.path(),
                CoderSession::FreshPersistent,
                "assigned Task prompt",
            )
            .is_err()
    );
}

#[cfg(unix)]
#[test]
fn native_terminal_controls_assigned_invocation() {
    use fluent::native_agent::relay_codex_streams;
    use std::io::{BufRead, BufReader, Write};
    use std::net::Shutdown;
    use std::os::unix::net::UnixStream;
    use std::sync::mpsc;

    let (mut tui, relay_client) = UnixStream::pair().unwrap();
    let (relay_server, mut provider) = UnixStream::pair().unwrap();
    tui.set_read_timeout(Some(std::time::Duration::from_secs(2)))
        .unwrap();
    provider
        .set_read_timeout(Some(std::time::Duration::from_secs(2)))
        .unwrap();
    let evidence = tempfile::NamedTempFile::new().unwrap();
    let evidence_path = evidence.path().to_path_buf();
    let (completed, received) = mpsc::channel();
    let relay = std::thread::spawn(move || {
        relay_codex_streams(
            relay_client,
            relay_server,
            "generation-1",
            "assigned-thread",
            evidence.reopen().unwrap(),
            move |completion| completed.send(completion).map_err(anyhow::Error::new),
        )
    });

    tui.write_all(
        b"{\"id\":1,\"method\":\"thread/resume\",\"params\":{\"threadId\":\"assigned-thread\"}}\n",
    )
    .unwrap();
    let mut provider_lines = BufReader::new(provider.try_clone().unwrap());
    let mut line = String::new();
    provider_lines.read_line(&mut line).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&line).unwrap()["params"]["threadId"],
        "assigned-thread"
    );
    tui.write_all(b"{\"id\":2,\"method\":\"turn/start\",\"params\":{\"threadId\":\"assigned-thread\",\"input\":[]}}\n").unwrap();
    line.clear();
    provider_lines.read_line(&mut line).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&line).unwrap()["method"],
        "turn/start"
    );

    let started = "{\"method\":\"turn/started\",\"params\":{\"threadId\":\"assigned-thread\",\"turn\":{\"id\":\"turn-1\"}}}\n";
    let finished = "{\"method\":\"turn/completed\",\"params\":{\"threadId\":\"assigned-thread\",\"turn\":{\"id\":\"turn-1\",\"status\":\"completed\"}}}\n";
    provider.write_all(started.as_bytes()).unwrap();
    provider.write_all(finished.as_bytes()).unwrap();
    let mut tui_lines = BufReader::new(tui.try_clone().unwrap());
    line.clear();
    tui_lines.read_line(&mut line).unwrap();
    assert_eq!(line, started);
    line.clear();
    tui_lines.read_line(&mut line).unwrap();
    assert_eq!(line, finished);
    assert_eq!(
        received
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap(),
        Completion::Accepted {
            generation: "generation-1".into(),
            turn_id: "turn-1".into(),
            input_sequence: 1,
        }
    );
    let evidence: Vec<serde_json::Value> = std::fs::read_to_string(evidence_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(evidence.len(), 2);
    assert_eq!(
        evidence[0]["event"],
        serde_json::from_str::<serde_json::Value>(started).unwrap()
    );
    assert_eq!(
        evidence[1]["event"],
        serde_json::from_str::<serde_json::Value>(finished).unwrap()
    );

    tui.write_all(b"{\"id\":3,\"method\":\"turn/start\",\"params\":{\"threadId\":\"other-thread\",\"input\":[]}}\n").unwrap();
    line.clear();
    assert_eq!(provider_lines.read_line(&mut line).unwrap(), 0);
    let error = relay.join().unwrap().unwrap_err();
    assert!(error.to_string().contains("another Task conversation"));
    let _ = tui.shutdown(Shutdown::Both);
    let _ = provider.shutdown(Shutdown::Both);
}

#[cfg(unix)]
#[test]
fn codex_relay_provider_exit_releases_attached_client() {
    use fluent::native_agent::relay_codex_streams;
    use std::os::unix::net::UnixStream;
    use std::sync::mpsc;

    let (_client, relay_client) = UnixStream::pair().unwrap();
    let (relay_server, provider) = UnixStream::pair().unwrap();
    let (finished, result) = mpsc::channel();
    let relay = std::thread::spawn(move || {
        let outcome = relay_codex_streams(
            relay_client,
            relay_server,
            "generation-1",
            "assigned-thread",
            Vec::<u8>::new(),
            |_| Ok(()),
        );
        let _ = finished.send(outcome);
    });
    drop(provider);
    if result
        .recv_timeout(std::time::Duration::from_secs(2))
        .is_err()
    {
        panic!("provider exit left the Codex relay waiting for client input");
    }
    relay.join().unwrap();
}

#[cfg(unix)]
#[test]
fn slow_display_cannot_block_evidence_or_supervision() {
    use fluent::native_agent::relay_codex_streams;
    use std::io::{BufRead, BufReader, Write};
    use std::net::Shutdown;
    use std::os::unix::net::UnixStream;
    use std::sync::mpsc;
    use std::time::Duration;

    let (mut tui, relay_client) = UnixStream::pair().unwrap();
    let (relay_server, mut provider) = UnixStream::pair().unwrap();
    provider
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let evidence = tempfile::NamedTempFile::new().unwrap();
    let evidence_path = evidence.path().to_path_buf();
    let (completed, received) = mpsc::channel();
    let relay = std::thread::spawn(move || {
        relay_codex_streams(
            relay_client,
            relay_server,
            "generation-1",
            "assigned-thread",
            evidence.reopen().unwrap(),
            move |completion| completed.send(completion).map_err(anyhow::Error::new),
        )
    });
    tui.write_all(b"{\"id\":1,\"method\":\"turn/start\",\"params\":{\"threadId\":\"assigned-thread\",\"input\":[]}}\n").unwrap();
    let mut request = String::new();
    BufReader::new(provider.try_clone().unwrap())
        .read_line(&mut request)
        .unwrap();
    assert!(request.contains("turn/start"));
    let started = "{\"method\":\"turn/started\",\"params\":{\"threadId\":\"assigned-thread\",\"turn\":{\"id\":\"turn-1\"}}}\n";
    provider.write_all(started.as_bytes()).unwrap();
    let bulk = format!(
        "{}\n",
        json!({"method": "item/updated", "params": {"text": "x".repeat(32768)}})
    );
    for _ in 0..64 {
        provider.write_all(bulk.as_bytes()).unwrap();
    }
    let finished = "{\"method\":\"turn/completed\",\"params\":{\"threadId\":\"assigned-thread\",\"turn\":{\"id\":\"turn-1\",\"status\":\"completed\"}}}\n";
    provider.write_all(finished.as_bytes()).unwrap();
    assert_eq!(
        received.recv_timeout(Duration::from_secs(2)).unwrap(),
        Completion::Accepted {
            generation: "generation-1".into(),
            turn_id: "turn-1".into(),
            input_sequence: 1,
        }
    );
    let captured: Vec<serde_json::Value> = std::fs::read_to_string(evidence_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(captured.len(), 66);
    assert_eq!(captured[0]["event"]["method"], "turn/started");
    assert_eq!(
        captured.last().unwrap()["event"]["method"],
        "turn/completed"
    );
    assert_eq!(
        captured
            .iter()
            .filter(|row| row["event"]["method"] == "item/updated")
            .count(),
        64
    );
    let _ = tui.shutdown(Shutdown::Both);
    drop(provider);
    relay.join().unwrap().unwrap();
}

#[cfg(unix)]
#[test]
fn native_capability_or_evidence_failure_blocks_false_success() {
    use fluent::native_agent::relay_codex_streams;
    use std::io::{self, BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    use std::sync::mpsc;

    struct FailedEvidence;
    impl Write for FailedEvidence {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("evidence volume unavailable"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let (mut tui, relay_client) = UnixStream::pair().unwrap();
    let (relay_server, mut provider) = UnixStream::pair().unwrap();
    let (completed, received) = mpsc::channel();
    let relay = std::thread::spawn(move || {
        relay_codex_streams(
            relay_client,
            relay_server,
            "generation-1",
            "assigned-thread",
            FailedEvidence,
            move |completion| completed.send(completion).map_err(anyhow::Error::new),
        )
    });
    tui.write_all(b"{\"id\":1,\"method\":\"turn/start\",\"params\":{\"threadId\":\"assigned-thread\",\"input\":[]}}\n").unwrap();
    let mut request = String::new();
    BufReader::new(provider.try_clone().unwrap())
        .read_line(&mut request)
        .unwrap();
    assert!(request.contains("turn/start"));
    provider.write_all(b"{\"method\":\"turn/started\",\"params\":{\"threadId\":\"assigned-thread\",\"turn\":{\"id\":\"turn-1\"}}}\n").unwrap();
    let error = relay.join().unwrap().unwrap_err();
    assert!(error.to_string().contains("evidence volume unavailable"));
    assert!(received.try_recv().is_err());
}

#[cfg(unix)]
#[test]
fn native_evidence_reaches_existing_consumers() {
    use fluent::native_agent::relay_codex_streams;
    use fluent::transcript::Event;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;

    let (mut tui, relay_client) = UnixStream::pair().unwrap();
    let (relay_server, mut provider) = UnixStream::pair().unwrap();
    let evidence = tempfile::NamedTempFile::new().unwrap();
    let evidence_path = evidence.path().to_path_buf();
    let evidence_writer = evidence.reopen().unwrap();
    let relay = std::thread::spawn(move || {
        relay_codex_streams(
            relay_client,
            relay_server,
            "generation-1",
            "assigned-thread",
            evidence_writer,
            |_| Ok(()),
        )
    });
    tui.write_all(b"{\"id\":1,\"method\":\"turn/start\",\"params\":{\"threadId\":\"assigned-thread\",\"input\":[]}}\n").unwrap();
    let mut request = String::new();
    BufReader::new(provider.try_clone().unwrap())
        .read_line(&mut request)
        .unwrap();
    assert!(request.contains("turn/start"));
    for event in [
        json!({"method":"thread/started","params":{"thread":{"id":"assigned-thread","model":"gpt-test"}}}),
        json!({"method":"turn/started","params":{"threadId":"assigned-thread","turn":{"id":"turn-1"}}}),
        json!({"method":"item/completed","params":{"threadId":"assigned-thread","turnId":"turn-1","item":{"id":"message-1","type":"agentMessage","text":"Task answer"}}}),
        json!({"method":"thread/tokenUsage/updated","params":{"threadId":"assigned-thread","turnId":"turn-1","tokenUsage":{"last":{"inputTokens":12,"outputTokens":7,"cachedInputTokens":3,"reasoningOutputTokens":2},"total":{"inputTokens":12,"outputTokens":7,"cachedInputTokens":3,"reasoningOutputTokens":2}}}}),
        json!({"method":"turn/completed","params":{"threadId":"assigned-thread","turn":{"id":"turn-1","status":"completed"}}}),
    ] {
        provider.write_all(event.to_string().as_bytes()).unwrap();
        provider.write_all(b"\n").unwrap();
    }
    drop(provider);
    relay.join().unwrap().unwrap();

    let lines: Vec<String> = std::fs::read_to_string(&evidence_path)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(lines.len(), 5);
    let first: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
    assert_eq!(first["format"], "fluent-native-agent-event");
    assert_eq!(first["schema_version"], 1);
    assert_eq!(first["provider"], "codex");
    assert_eq!(first["execution_generation"], "generation-1");
    assert_eq!(first["assigned_conversation_id"], "assigned-thread");
    assert_eq!(first["conversation_id"], "assigned-thread");
    assert_eq!(first["event"]["method"], "thread/started");
    assert_eq!(first["input_sequence"], 1);
    let completed: serde_json::Value = serde_json::from_str(lines.last().unwrap()).unwrap();
    assert_eq!(completed["turn_id"], "turn-1");
    assert_eq!(completed["event"]["method"], "turn/completed");

    let events: Vec<Event> = lines
        .iter()
        .flat_map(|line| fluent::transcript::parse_line(line))
        .collect();
    assert!(events.iter().any(|event| matches!(event, Event::SessionInit { session_id, .. } if session_id == "assigned-thread")));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::Text { text } if text == "Task answer"))
    );
    let rows = fluent::usage::extract_codex_usage(&evidence_path, "work-1", "attempt-1", "write-1");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].model, "gpt-test");
    assert_eq!(rows[0].input_tokens, 12);
    assert_eq!(rows[0].output_tokens, 7);
    assert!(matches!(
        fluent::transcript::parse_line(
            r#"{"type":"thread.started","thread_id":"legacy-thread","provider":"codex"}"#
        ).as_slice(),
        [Event::SessionInit { session_id, .. }] if session_id == "legacy-thread"
    ));
}

#[test]
fn continuation_reuses_conversation_not_live_terminal_identity() {
    let make_record = |generation: &str| {
        json!({
            "format": "fluent-native-agent-event",
            "schema_version": 1,
            "provider": "codex",
            "execution_generation": generation,
            "assigned_conversation_id": "assigned-thread",
            "conversation_id": "assigned-thread",
            "turn_id": "turn-2",
            "input_sequence": 1,
            "event": {"method": "turn/completed", "params": {"threadId": "assigned-thread", "turn": {"id": "turn-2", "status": "completed"}}}
        }).to_string()
    };
    let initial = make_record("generation-1");
    let resumed = make_record("generation-2");
    assert_eq!(
        fluent::transcript::conversation_id_from_line(&initial).as_deref(),
        Some("assigned-thread")
    );
    assert_eq!(
        fluent::transcript::conversation_id_from_line(&resumed).as_deref(),
        Some("assigned-thread")
    );
    assert_ne!(
        serde_json::from_str::<serde_json::Value>(&initial).unwrap()["execution_generation"],
        serde_json::from_str::<serde_json::Value>(&resumed).unwrap()["execution_generation"]
    );
    let mut unrelated: serde_json::Value = serde_json::from_str(&resumed).unwrap();
    unrelated["conversation_id"] = json!("other-thread");
    unrelated["event"]["params"]["threadId"] = json!("other-thread");
    assert_eq!(
        fluent::transcript::conversation_id_from_line(&unrelated.to_string()).as_deref(),
        Some("assigned-thread")
    );

    let mut resumed_gate = CodexRelayGate::new("generation-2", "assigned-thread");
    resumed_gate.admit_request(&json!({"id":1,"method":"turn/start","params":{"threadId":"assigned-thread","input":[]}})).unwrap();
    let started = json!({"method":"turn/started","params":{"threadId":"assigned-thread","turn":{"id":"turn-2"}}});
    assert_eq!(
        resumed_gate
            .observe_event_for_generation("generation-1", &started)
            .unwrap(),
        Completion::Ignored
    );
    assert_eq!(
        resumed_gate
            .observe_event_for_generation("generation-2", &started)
            .unwrap(),
        Completion::Ignored
    );
    let finished = json!({"method":"turn/completed","params":{"threadId":"assigned-thread","turn":{"id":"turn-2","status":"completed"}}});
    assert_eq!(
        resumed_gate
            .observe_event_for_generation("generation-1", &finished)
            .unwrap(),
        Completion::Ignored
    );
    assert_eq!(
        resumed_gate
            .observe_event_for_generation("generation-2", &finished)
            .unwrap(),
        Completion::Accepted {
            generation: "generation-2".into(),
            turn_id: "turn-2".into(),
            input_sequence: 1
        }
    );
}
