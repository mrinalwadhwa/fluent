use fluent::native_agent::{CodexRelayGate, Completion, NativeAdmission};
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
