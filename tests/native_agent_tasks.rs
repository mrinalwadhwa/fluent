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
fn codex_steering_must_be_acknowledged_before_settlement() {
    let mut gate = CodexRelayGate::new("generation-1", "assigned-thread");
    gate.admit_request(
        &json!({"id":1,"method":"turn/start","params":{"threadId":"assigned-thread","input":[]}}),
    )
    .unwrap();
    gate.observe_event_for_generation("generation-1", &json!({"method":"turn/started","params":{"threadId":"assigned-thread","turn":{"id":"turn-1"}}})).unwrap();
    gate.admit_request(
        &json!({"id":2,"method":"turn/steer","params":{"threadId":"assigned-thread","input":[]}}),
    )
    .unwrap();
    let completed = json!({"method":"turn/completed","params":{"threadId":"assigned-thread","turn":{"id":"turn-1","status":"completed"}}});
    assert_eq!(
        gate.observe_event_for_generation("generation-1", &completed)
            .unwrap(),
        Completion::Ignored
    );
    assert!(
        gate.observe_event_for_generation(
            "generation-1",
            &json!({"id":2,"error":{"message":"rejected"}})
        )
        .is_err()
    );

    let mut acknowledged = CodexRelayGate::new("generation-2", "assigned-thread");
    acknowledged.admit_request(&json!({"id":1,"method":"turn/start","params":{"threadId":"assigned-thread","input":[]}})).unwrap();
    acknowledged.observe_event_for_generation("generation-2", &json!({"method":"turn/started","params":{"threadId":"assigned-thread","turn":{"id":"turn-2"}}})).unwrap();
    acknowledged.admit_request(&json!({"id":2,"method":"turn/steer","params":{"threadId":"assigned-thread","input":[]}})).unwrap();
    acknowledged
        .observe_event_for_generation("generation-2", &json!({"id":2,"result":{}}))
        .unwrap();
    assert_eq!(acknowledged.observe_event_for_generation("generation-2", &json!({"method":"turn/completed","params":{"threadId":"assigned-thread","turn":{"id":"turn-2","status":"completed"}}})).unwrap(), Completion::Accepted { generation: "generation-2".into(), turn_id: "turn-2".into(), input_sequence: 2 });
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

    let raced = NativeAdmission::new();
    assert_eq!(raced.admit().unwrap(), 1);
    // A new turn admitted after a completion candidate invalidates that
    // candidate without closing the invocation's input boundary.
    assert_eq!(raced.admit().unwrap(), 2);
    assert!(!raced.settle_if_current(1).unwrap());
    assert_eq!(raced.admit().unwrap(), 3);
    assert!(raced.settle_if_current(3).unwrap());
    assert!(raced.admit().is_err());

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

#[test]
fn pi_extension_reports_admitted_entries_before_settlement() {
    let script = r#"
import assert from 'node:assert/strict';
import { createFluentPiExtension } from './src/native_agent/pi_extension.mjs';
const callbacks = new Map();
const messages = [];
const manager = {
  getSessionId: () => 'assigned-session',
  getSessionFile: () => '/private/task/session.jsonl',
  getEntries: () => entries,
};
const ctx = { sessionManager: manager };
let entries = [];
createFluentPiExtension(async (message) => {
  messages.push(message);
  return { ok: true, input_sequence: message.event.type === 'input' ? messages.filter(item => item.event.type === 'input').length : undefined };
})({ on: (kind, callback) => callbacks.set(kind, callback) });
await callbacks.get('session_start')({ type: 'session_start', reason: 'startup' }, ctx);
assert.deepEqual(await callbacks.get('input')({ type: 'input', text: 'first', streamingBehavior: 'steer' }, ctx), { action: 'continue' });
assert.deepEqual(await callbacks.get('input')({ type: 'input', text: 'second', streamingBehavior: 'followUp' }, ctx), { action: 'continue' });
entries = [
  { type: 'message', id: 'entry-1', message: { role: 'user' } },
  { type: 'message', id: 'entry-2', message: { role: 'user' } },
];
await callbacks.get('agent_before_settle')({ type: 'agent_before_settle', outcome: 'completed' }, ctx);
await callbacks.get('agent_settled')({ type: 'agent_settled', aborted: false }, ctx);
assert.deepEqual(messages.slice(-3).map(item => item.event.type), ['input_entry', 'input_entry', 'agent_settled']);
assert.deepEqual(messages.slice(-3, -1).map(item => item.input_sequence), [1, 2]);
assert.equal(callbacks.get('session_before_switch')().cancel, true);
assert.equal(callbacks.get('session_before_fork')().cancel, true);
assert.equal(callbacks.get('session_before_tree')().cancel, true);
assert.equal(callbacks.get('user_bash')().result.exitCode, 1);
await assert.rejects(callbacks.get('session_start')({ type: 'session_start', reason: 'reload' }, ctx));
"#;
    let output = std::process::Command::new("node")
        .args(["--input-type=module", "-e", script])
        .env("FLUENT_NATIVE_PI_GENERATION", "generation-1")
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
#[test]
fn pi_receiver_acknowledges_durable_input_and_settlement() {
    use fluent::native_agent::pi::{PiEventGate, receive_event};
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;

    let evidence = tempfile::NamedTempFile::new().unwrap();
    let mut writer = evidence.reopen().unwrap();
    let mut gate = PiEventGate::new("generation-1", "/private/task/session.jsonl");
    let mut observed = Vec::new();
    for event in [
        json!({"type":"session_start","reason":"startup"}),
        json!({"type":"input","text":"first","streamingBehavior":"steer"}),
        json!({"type":"input","text":"second","streamingBehavior":"followUp"}),
        json!({"type":"input_entry","entry_id":"entry-1"}),
        json!({"type":"input_entry","entry_id":"entry-2"}),
        json!({"type":"agent_before_settle","outcome":"completed"}),
        json!({"type":"agent_settled","aborted":false}),
    ] {
        let (mut client, receiver) = UnixStream::pair().unwrap();
        let mut envelope = json!({"generation":"generation-1","session_id":"assigned-session","session_file":"/private/task/session.jsonl","event":event});
        if envelope["event"]["entry_id"] == "entry-1" {
            envelope["input_sequence"] = json!(1);
        }
        if envelope["event"]["entry_id"] == "entry-2" {
            envelope["input_sequence"] = json!(2);
        }
        client
            .write_all(format!("{}\n", envelope).as_bytes())
            .unwrap();
        let completion = receive_event(receiver, &mut gate, &mut writer).unwrap();
        let mut reply = String::new();
        BufReader::new(client).read_line(&mut reply).unwrap();
        let reply: serde_json::Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(reply["ok"], true);
        observed.push((
            event["type"].as_str().unwrap().to_string(),
            reply["input_sequence"].clone(),
            completion,
        ));
    }
    assert_eq!(observed[1].1, json!(1));
    assert_eq!(observed[2].1, json!(2));
    assert!(matches!(
        observed.last().unwrap().2,
        Completion::Accepted {
            input_sequence: 2,
            ..
        }
    ));
    let saved = std::fs::read_to_string(evidence.path()).unwrap();
    assert_eq!(saved.lines().count(), 7);
    assert!(saved.contains("entry-2"));
    assert!(gate.observe(&json!({"generation":"generation-old","session_id":"assigned-session","session_file":"/private/task/session.jsonl","event":{"type":"agent_settled","aborted":false}})).is_err());
}

#[test]
fn pi_native_model_config_keeps_only_assigned_provider() {
    use fluent::native_agent::pi::select_local_model_config;
    let source = json!({"providers": {
        "local-openai": {"baseUrl":"http://localhost:1234/v1","api":"openai-completions","apiKey":"local","models":[{"id":"assigned-model"},{"id":"other-model"}]},
        "remote": {"baseUrl":"https://example.invalid","models":[{"id":"remote-model"}]}
    }});
    let selected = select_local_model_config(&source, "assigned-model").unwrap();
    assert_eq!(
        selected["providers"]["local-openai"]["baseUrl"],
        "http://localhost:1234/v1"
    );
    assert_eq!(
        selected["providers"]["local-openai"]["models"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(selected["providers"].get("remote").is_none());
    assert!(select_local_model_config(&source, "unknown-model").is_err());
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
    use fluent::coder::{CoderSession, CodexCode, PiCode, SandboxedClaudeCode};
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
    let remote = private_home.path().join("relay.sock");
    let remote_command = coder
        .native_task_command_with_remote(
            project.path(),
            private_home.path(),
            CoderSession::Resume("assigned-session"),
            "continue Task",
            Some(&remote),
        )
        .unwrap();
    let remote_args: Vec<_> = remote_command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let remote_uri = format!("unix://{}", remote.display());
    assert!(
        remote_args
            .windows(2)
            .any(|args| { args[0] == "--remote" && args[1] == remote_uri })
    );
    assert!(
        remote_args
            .windows(2)
            .any(|args| args == ["resume", "assigned-session"])
    );

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

    let settings = private_home.path().join("owned-settings.json");
    std::fs::write(&settings, "{}").unwrap();
    let claude = SandboxedClaudeCode {
        sandbox_profile: None,
        trusted_sandbox: false,
        model_override: Some("claude-assigned".into()),
        effort: Some("high".into()),
    };
    let native = claude
        .native_task_command_in_terminal(
            project.path(),
            private_home.path(),
            &settings,
            CoderSession::Resume("assigned-claude-session"),
            "role instructions",
            "Task prompt",
        )
        .unwrap();
    let args: Vec<_> = native
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert_eq!(native.get_program(), OsStr::new("claude"));
    for pair in [
        ["--model", "claude-assigned"],
        ["--effort", "high"],
        ["--setting-sources", ""],
        ["--resume", "assigned-claude-session"],
        ["--append-system-prompt", "role instructions"],
    ] {
        assert!(args.windows(2).any(|window| window == pair));
    }
    assert_eq!(args.last().unwrap(), "Task prompt");
    assert!(
        native
            .get_envs()
            .any(|(key, value)| key == OsStr::new("CLAUDE_CONFIG_DIR")
                && value == Some(private_home.path().as_os_str()))
    );

    let extension = private_home.path().join("fluent-pi-extension.mjs");
    std::fs::write(&extension, "export default () => {};").unwrap();
    let session_file = private_home.path().join("assigned-session.jsonl");
    let pi = PiCode {
        sandbox_profile: None,
        trusted_sandbox: false,
        model_override: Some("assigned-local-model".into()),
    };
    let pi_command = pi
        .native_task_command_in_terminal(
            project.path(),
            private_home.path(),
            &extension,
            &session_file,
            "high",
            "role instructions",
            "Task prompt",
        )
        .unwrap();
    let pi_args: Vec<_> = pi_command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert_eq!(pi_command.get_program(), OsStr::new("pi"));
    for pair in [
        ["--provider", "local-openai"],
        ["--model", "assigned-local-model"],
        ["--thinking", "high"],
        ["--append-system-prompt", "role instructions"],
    ] {
        assert!(pi_args.windows(2).any(|window| window == pair));
    }
    assert!(pi_args.contains(&"--no-extensions".to_string()));
    assert!(pi_args.contains(&"--approve".to_string()));
    assert!(
        pi_args.windows(2).any(
            |window| window[0] == "--extension" && window[1] == extension.display().to_string()
        )
    );
    assert!(
        pi_args
            .windows(2)
            .any(|window| window[0] == "--session"
                && window[1] == session_file.display().to_string())
    );
    assert_eq!(pi_args.last().unwrap(), "Task prompt");
    assert!(
        pi_command
            .get_envs()
            .any(|(key, value)| key == OsStr::new("PI_CODING_AGENT_DIR")
                && value == Some(private_home.path().as_os_str()))
    );
}

#[test]
fn native_codex_pane_loads_the_assigned_thread() {
    use fluent::coder::CodexCode;
    use fluent::native_agent::codex::pane_script;

    let root = tempfile::tempdir().unwrap();
    let coder = CodexCode {
        sandbox_profile: None,
        trusted_sandbox: false,
        model_override: Some("gpt-assigned".into()),
        effort: Some("high".into()),
        executable: root.path().join("codex"),
    };
    let script = pane_script(
        &coder,
        root.path(),
        root.path(),
        &root.path().join("app.sock"),
        &root.path().join("relay.sock"),
        &root.path().join("go"),
        &root.path().join("thread"),
        &root.path().join("provider.log"),
        "assigned Task prompt",
    )
    .unwrap();
    assert!(script.contains("'app-server' '--listen'"));
    assert!(script.contains("--remote"));
    assert!(script.contains("'resume' \"$assigned_thread\""));
    assert!(script.contains("--model"));
    assert!(script.contains("gpt-assigned"));
    assert!(script.contains("model_reasoning_effort=high"));
    assert!(script.contains("--disable"));
    assert!(script.contains("hooks"));
    assert!(script.contains("assigned Task prompt"));
    assert_eq!(script.matches("__FLUENT_ASSIGNED_THREAD__").count(), 0);
}

#[test]
fn native_claude_task_uses_owned_settings_and_role() {
    use fluent::coder::{CoderSession, SandboxedClaudeCode};
    use std::ffi::OsStr;

    let project = tempfile::tempdir().unwrap();
    let private_home = tempfile::tempdir().unwrap();
    let settings = private_home.path().join("native-settings.json");
    std::fs::write(&settings, r#"{"hooks":{"SessionStart":[]}}"#).unwrap();
    let coder = SandboxedClaudeCode {
        sandbox_profile: Some("(version 1) (allow default)".into()),
        trusted_sandbox: false,
        model_override: Some("claude-assigned".into()),
        effort: Some("high".into()),
    };
    let command = coder
        .native_task_command(
            project.path(),
            private_home.path(),
            &settings,
            CoderSession::Resume("assigned-session"),
            "assigned role instructions",
            "assigned Task prompt",
        )
        .unwrap();
    assert_eq!(command.get_program(), OsStr::new("sandbox-exec"));
    let args: Vec<_> = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert!(
        args.windows(2)
            .any(|args| args == ["-f", "(version 1) (allow default)"])
    );
    assert!(
        args.windows(2)
            .any(|args| args == ["--model", "claude-assigned"])
    );
    assert!(args.windows(2).any(|args| args == ["--effort", "high"]));
    assert!(
        args.windows(2)
            .any(|args| args == ["--setting-sources", ""])
    );
    assert!(
        args.windows(2)
            .any(|args| args == ["--settings", settings.to_str().unwrap()])
    );
    assert!(
        args.windows(2)
            .any(|args| args == ["--resume", "assigned-session"])
    );
    assert!(
        args.windows(2)
            .any(|args| args == ["--append-system-prompt", "assigned role instructions"])
    );
    assert!(args.contains(&"--strict-mcp-config".to_string()));
    assert!(!args.contains(&"--safe-mode".to_string()));
    assert!(!args.contains(&"-p".to_string()));
    assert_eq!(args.last().unwrap(), "assigned Task prompt");
    for name in ["HOME", "CLAUDE_CONFIG_DIR"] {
        assert!(command.get_envs().any(|(key, value)| {
            key == OsStr::new(name) && value == Some(private_home.path().as_os_str())
        }));
    }

    let outside_settings = tempfile::NamedTempFile::new().unwrap();
    assert!(
        coder
            .native_task_command(
                project.path(),
                private_home.path(),
                outside_settings.path(),
                CoderSession::Ephemeral,
                "role",
                "prompt"
            )
            .is_err()
    );
    let missing_model = SandboxedClaudeCode {
        model_override: None,
        ..coder
    };
    assert!(
        missing_model
            .native_task_command(
                project.path(),
                private_home.path(),
                &settings,
                CoderSession::Ephemeral,
                "role",
                "prompt"
            )
            .is_err()
    );
}

#[test]
fn native_claude_hooks_correlate_prompt_and_session() {
    use fluent::native_agent::claude::ClaudeHookGate;

    let mut gate = ClaudeHookGate::new("generation-1", Some("assigned-session"));
    assert!(gate.observe("generation-1", &json!({"hook_event_name":"SessionStart","session_id":"other-session","source":"resume"})).is_err());
    gate.observe("generation-1", &json!({"hook_event_name":"SessionStart","session_id":"assigned-session","source":"resume"})).unwrap();
    gate.observe("generation-1", &json!({"hook_event_name":"UserPromptSubmit","session_id":"assigned-session","prompt_id":"prompt-1","prompt":"work"})).unwrap();
    gate.observe("generation-1", &json!({"hook_event_name":"UserPromptSubmit","session_id":"assigned-session","prompt_id":"prompt-2","prompt":"follow up"})).unwrap();
    let old = json!({"hook_event_name":"Stop","session_id":"assigned-session","prompt_id":"prompt-1","last_assistant_message":"older answer","background_tasks":[],"session_crons":[]});
    assert_eq!(
        gate.observe("generation-1", &old).unwrap(),
        Completion::Ignored
    );
    let current = json!({"hook_event_name":"Stop","session_id":"assigned-session","prompt_id":"prompt-2","last_assistant_message":"final answer","background_tasks":[],"session_crons":[]});
    assert_eq!(
        gate.observe("generation-1", &current).unwrap(),
        Completion::Accepted {
            generation: "generation-1".into(),
            turn_id: "prompt-2".into(),
            input_sequence: 2,
        }
    );
    assert!(gate.observe("generation-1", &json!({"hook_event_name":"UserPromptSubmit","session_id":"assigned-session","prompt_id":"prompt-3"})).is_err());
    assert!(gate.observe("generation-2", &current).is_err());

    let mut failed = ClaudeHookGate::new("generation-1", None);
    failed.observe("generation-1", &json!({"hook_event_name":"SessionStart","session_id":"assigned-session","source":"startup"})).unwrap();
    failed.observe("generation-1", &json!({"hook_event_name":"UserPromptSubmit","session_id":"assigned-session","prompt_id":"prompt-1"})).unwrap();
    assert!(failed.observe("generation-1", &json!({"hook_event_name":"StopFailure","session_id":"assigned-session","prompt_id":"prompt-1","error":"rate_limit"})).is_err());
}

#[cfg(unix)]
#[test]
fn codex_jsonl_relay_controls_assigned_invocation() {
    use fluent::native_agent::relay_codex_streams_with_admission;
    use std::io::{BufRead, BufReader, Write};
    use std::net::Shutdown;
    use std::os::unix::net::UnixStream;
    use std::sync::Arc;
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
    let admission = Arc::new(NativeAdmission::new());
    let relay = std::thread::spawn(move || {
        let settlement = Arc::clone(&admission);
        relay_codex_streams_with_admission(
            relay_client,
            relay_server,
            "generation-1",
            "assigned-thread",
            admission,
            evidence.reopen().unwrap(),
            move |completion| {
                if let Completion::Accepted { input_sequence, .. } = &completion {
                    assert!(settlement.settle_if_current(*input_sequence)?);
                }
                completed.send(completion).map_err(anyhow::Error::new)
            },
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

    // Completion closes this invocation. A later turn addressed to the same
    // conversation must not cross the relay into the provider.
    tui.write_all(b"{\"id\":3,\"method\":\"turn/start\",\"params\":{\"threadId\":\"assigned-thread\",\"input\":[]}}\n").unwrap();
    line.clear();
    assert_eq!(provider_lines.read_line(&mut line).unwrap(), 0);
    let error = relay.join().unwrap().unwrap_err();
    assert!(error.to_string().contains("input is closed"));
    let _ = tui.shutdown(Shutdown::Both);
    let _ = provider.shutdown(Shutdown::Both);
}

#[cfg(unix)]
#[test]
fn native_terminal_controls_assigned_invocation() {
    use fluent::native_agent::codex::relay_codex_websockets;
    use std::net::Shutdown;
    use std::os::unix::net::UnixStream;
    use std::sync::{Arc, mpsc};
    use std::time::Duration;
    use tungstenite::Message;

    let (tui_stream, relay_client) = UnixStream::pair().unwrap();
    let (relay_server, provider_stream) = UnixStream::pair().unwrap();
    let evidence = tempfile::NamedTempFile::new().unwrap();
    let evidence_path = evidence.path().to_path_buf();
    let provider = std::thread::spawn(move || {
        let mut socket = tungstenite::accept(provider_stream).unwrap();
        socket
            .get_ref()
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let resume: serde_json::Value =
            serde_json::from_str(&socket.read().unwrap().into_text().unwrap()).unwrap();
        assert_eq!(resume["method"], "thread/resume");
        assert_eq!(resume["params"]["threadId"], "assigned-thread");
        let turn: serde_json::Value =
            serde_json::from_str(&socket.read().unwrap().into_text().unwrap()).unwrap();
        assert_eq!(turn["method"], "turn/start");
        assert_eq!(turn["params"]["threadId"], "assigned-thread");
        for event in [
            json!({"method":"turn/started","params":{"threadId":"assigned-thread","turn":{"id":"turn-1"}}}),
            json!({"method":"turn/completed","params":{"threadId":"assigned-thread","turn":{"id":"turn-1","status":"completed"}}}),
        ] {
            socket.send(Message::Text(event.to_string())).unwrap();
        }
        socket
            .get_ref()
            .set_read_timeout(Some(Duration::from_millis(300)))
            .unwrap();
        let late = socket.read();
        assert!(late.is_err(), "a late turn crossed the assigned relay");
    });
    let admission = Arc::new(NativeAdmission::new());
    let (completed, received) = mpsc::channel();
    let relay = std::thread::spawn(move || {
        let client = tungstenite::accept(relay_client).unwrap();
        let (server, _) = tungstenite::client("ws://localhost/", relay_server).unwrap();
        let settlement = Arc::clone(&admission);
        relay_codex_websockets(
            client,
            server,
            "generation-1",
            "assigned-thread",
            admission,
            evidence.reopen().unwrap(),
            move |completion| {
                if let Completion::Accepted { input_sequence, .. } = &completion {
                    assert!(settlement.settle_if_current(*input_sequence)?);
                }
                completed.send(completion).map_err(anyhow::Error::new)
            },
        )
    });
    let (mut tui, _) = tungstenite::client("ws://localhost/", tui_stream).unwrap();
    tui.get_ref()
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    tui.send(Message::Text(
        json!({"id":1,"method":"thread/resume","params":{"threadId":"assigned-thread"}})
            .to_string(),
    ))
    .unwrap();
    tui.send(Message::Text(
        json!({"id":2,"method":"turn/start","params":{"threadId":"assigned-thread","input":[]}})
            .to_string(),
    ))
    .unwrap();
    for method in ["turn/started", "turn/completed"] {
        let event: serde_json::Value =
            serde_json::from_str(&tui.read().unwrap().into_text().unwrap()).unwrap();
        assert_eq!(event["method"], method);
    }
    assert_eq!(
        received.recv_timeout(Duration::from_secs(2)).unwrap(),
        Completion::Accepted {
            generation: "generation-1".into(),
            turn_id: "turn-1".into(),
            input_sequence: 1,
        }
    );
    tui.send(Message::Text(
        json!({"id":3,"method":"turn/start","params":{"threadId":"assigned-thread","input":[]}})
            .to_string(),
    ))
    .unwrap();
    let rejection: serde_json::Value =
        serde_json::from_str(&tui.read().unwrap().into_text().unwrap()).unwrap();
    assert_eq!(rejection["id"], 3);
    assert!(
        rejection["error"]["message"]
            .as_str()
            .unwrap()
            .contains("input is closed")
    );
    let evidence: Vec<serde_json::Value> = std::fs::read_to_string(evidence_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(evidence.len(), 2);
    assert_eq!(evidence[0]["event"]["method"], "turn/started");
    assert_eq!(evidence[1]["event"]["method"], "turn/completed");
    let _ = tui.get_ref().shutdown(Shutdown::Both);
    provider.join().unwrap();
    let _ = relay.join().unwrap();
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
    // Display backpressure must not close the TUI's input half. A later
    // provider turn still belongs to this invocation.
    provider
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    tui.write_all(b"{\"id\":2,\"method\":\"turn/start\",\"params\":{\"threadId\":\"assigned-thread\",\"input\":[]}}\n")
        .unwrap();
    let mut next_request = String::new();
    BufReader::new(provider.try_clone().unwrap())
        .read_line(&mut next_request)
        .unwrap();
    assert!(next_request.contains("turn/start"));
    let _ = tui.shutdown(Shutdown::Both);
    drop(provider);
    relay.join().unwrap().unwrap();
}

#[cfg(unix)]
#[test]
fn native_capability_or_evidence_failure_blocks_false_success() {
    use fluent::native_agent::claude::{
        ClaudeHookGate, receive_hook, supports_blocking_hook_failure,
    };
    use fluent::native_agent::relay_codex_streams;
    use std::io::{self, BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    use std::sync::mpsc;

    struct FailedEvidence;
    assert!(!supports_blocking_hook_failure("2.1.289 (Claude Code)"));
    assert!(supports_blocking_hook_failure("2.1.295 (Claude Code)"));
    assert!(supports_blocking_hook_failure("2.2.0 (Claude Code)"));
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

    let evidence = tempfile::NamedTempFile::new().unwrap();
    let mut writer = evidence.reopen().unwrap();
    let mut gate = ClaudeHookGate::new("generation-1", Some("assigned-session"));
    for event in [
        json!({"hook_event_name":"SessionStart","session_id":"assigned-session","source":"resume"}),
        json!({"hook_event_name":"UserPromptSubmit","session_id":"assigned-session","prompt_id":"prompt-1","prompt":"work"}),
        json!({"hook_event_name":"Stop","session_id":"assigned-session","prompt_id":"prompt-1","last_assistant_message":"done","background_tasks":[],"session_crons":[]}),
    ] {
        let (mut hook, receiver) = UnixStream::pair().unwrap();
        hook.write_all(
            format!("{}\n", json!({"generation":"generation-1","event":event})).as_bytes(),
        )
        .unwrap();
        let completion = receive_hook(receiver, &mut gate, "generation-1", &mut writer).unwrap();
        let mut ack = String::new();
        BufReader::new(hook).read_line(&mut ack).unwrap();
        assert_eq!(ack, "ok\n");
        if event["hook_event_name"] == "Stop" {
            assert!(matches!(completion, Completion::Accepted { .. }));
        }
    }
    let saved = std::fs::read_to_string(evidence.path()).unwrap();
    assert_eq!(saved.lines().count(), 3);
    assert!(saved.contains("last_assistant_message"));
    let (mut hook, receiver) = UnixStream::pair().unwrap();
    hook.write_all(format!("{}\n", json!({"generation":"stale","event":{"hook_event_name":"SessionEnd","session_id":"assigned-session"}})).as_bytes()).unwrap();
    assert!(receive_hook(receiver, &mut gate, "generation-1", &mut writer).is_err());
    assert_eq!(
        std::fs::read_to_string(evidence.path())
            .unwrap()
            .lines()
            .count(),
        3
    );
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

    let claude = |event| {
        json!({
            "format":"fluent-native-agent-event","schema_version":1,"provider":"claude",
            "execution_generation":"generation-2","assigned_conversation_id":"assigned-claude",
            "conversation_id":"assigned-claude","turn_id":"prompt-1","input_sequence":1,
            "event":event,
        })
        .to_string()
    };
    let start = claude(
        json!({"hook_event_name":"SessionStart","session_id":"assigned-claude","source":"resume"}),
    );
    assert_eq!(
        fluent::transcript::conversation_id_from_line(&start).as_deref(),
        Some("assigned-claude")
    );
    assert!(
        matches!(fluent::transcript::parse_line(&start).as_slice(), [Event::SessionInit { session_id, .. }] if session_id == "assigned-claude")
    );
    let tool = claude(
        json!({"hook_event_name":"PreToolUse","session_id":"assigned-claude","tool_name":"Bash","tool_use_id":"tool-1","tool_input":{"command":"git status"}}),
    );
    assert!(
        matches!(fluent::transcript::parse_line(&tool).as_slice(), [Event::ToolUse { id, name, .. }] if id == "tool-1" && name == "Bash")
    );
    let stop = claude(
        json!({"hook_event_name":"Stop","session_id":"assigned-claude","prompt_id":"prompt-1","last_assistant_message":"Claude answer"}),
    );
    assert!(
        matches!(fluent::transcript::parse_line(&stop).as_slice(), [Event::Text { text }] if text == "Claude answer")
    );
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
