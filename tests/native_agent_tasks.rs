use fluent::native_agent::{CodexRelayGate, Completion, NativeAdmission, PiSettlementGate};
use fluent::work_model::{Attempt, CoderMapping, TerminalMode, WorkItem, reopen_attempt};
use serde_json::json;

fn native_input_event(record: &serde_json::Value) -> bool {
    matches!(
        record["event"]["method"].as_str(),
        Some("turn/start" | "turn/steer")
    ) || record["event"]["hook_event_name"] == "UserPromptSubmit"
        || record["event"]["type"] == "input"
}

fn race_generation_closed_after_attempt(
    attempt: &std::path::Path,
    descriptors: &[fluent::terminal_sessions::TerminalDescriptor],
    session: &str,
    generation: &str,
) -> bool {
    // A PTY send can succeed after its native input owner has closed. In that
    // ordering no provider callback exists to record admission or rejection.
    // The generation was captured from the live attachment before settlement;
    // its registry entry may be gone by the time the send result is recorded.
    let attempted = std::fs::read_to_string(attempt)
        .is_ok_and(|result| result.starts_with("0 ") || result.starts_with("1 "));
    attempted
        && match descriptors
            .iter()
            .find(|descriptor| descriptor.session_id == session)
        {
            Some(descriptor) => {
                descriptor.execution_generation == generation
                    && descriptor.lifecycle == fluent::terminal_sessions::TerminalLifecycle::Ended
            }
            None => true,
        }
}

#[test]
fn native_input_evidence_excludes_quoted_output() {
    let input = json!({"event":{"method":"turn/steer","params":{"input":[{"text":"probe"}]}},"input_sequence":2});
    let quote =
        json!({"event":{"method":"item/updated","params":{"text":"probe"}},"input_sequence":2});
    assert!(native_input_event(&input));
    assert!(!native_input_event(&quote));
}

#[test]
fn completion_race_accepts_closure_after_send_attempt() {
    use fluent::terminal_sessions::{TerminalDescriptor, TerminalLifecycle};

    let root = tempfile::tempdir().unwrap();
    let attempt = root.path().join("race.attempt");
    let descriptor: TerminalDescriptor = serde_json::from_value(json!({
        "schema_version": 1,
        "source_id": "source",
        "project_identity": "project",
        "work_item_id": "work",
        "attempt_id": "attempt-1",
        "task_id": "attempt-1-write-1",
        "execution_generation": "writer-generation",
        "session_id": "writer-session",
        "role": "Writer",
        "provider": "pi",
        "lifecycle": "running",
        "can_observe": true,
        "can_control": true
    }))
    .unwrap();
    let mut descriptors = vec![descriptor];

    // Control was occupied while Running; completion wins before Expect sends.
    assert!(!race_generation_closed_after_attempt(
        &attempt,
        &descriptors,
        "writer-session",
        "writer-generation"
    ));
    descriptors[0].lifecycle = TerminalLifecycle::Ended;
    assert!(!race_generation_closed_after_attempt(
        &attempt,
        &descriptors,
        "writer-session",
        "writer-generation"
    ));
    std::fs::write(&attempt, "0 ").unwrap();
    assert!(race_generation_closed_after_attempt(
        &attempt,
        &descriptors,
        "writer-session",
        "writer-generation"
    ));
    assert!(!race_generation_closed_after_attempt(
        &attempt,
        &descriptors,
        "writer-session",
        "reviewer-generation"
    ));
    descriptors[0].lifecycle = TerminalLifecycle::Running;
    assert!(!race_generation_closed_after_attempt(
        &attempt,
        &descriptors,
        "writer-session",
        "writer-generation"
    ));
    descriptors[0].lifecycle = TerminalLifecycle::Ended;
    std::fs::write(&attempt, "1 attachment closed").unwrap();
    assert!(race_generation_closed_after_attempt(
        &attempt,
        &descriptors,
        "writer-session",
        "writer-generation"
    ));
    let mut reviewer = descriptors[0].clone();
    reviewer.session_id = "reviewer-session".into();
    reviewer.execution_generation = "reviewer-generation".into();
    reviewer.lifecycle = TerminalLifecycle::Running;
    descriptors.clear();
    assert!(race_generation_closed_after_attempt(
        &attempt,
        &descriptors,
        "writer-session",
        "writer-generation"
    ));
    descriptors.push(reviewer);
    assert!(race_generation_closed_after_attempt(
        &attempt,
        &descriptors,
        "writer-session",
        "writer-generation"
    ));
}

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
    gate.admit_request(&json!({"id": 2, "method": "turn/start", "params": {"threadId": "assigned-thread", "input": []}})).unwrap();
    let next_started = json!({"method": "turn/started", "params": {"threadId": "assigned-thread", "turn": {"id": "turn-2"}}});
    let next_completed = json!({"method": "turn/completed", "params": {"threadId": "assigned-thread", "turn": {"id": "turn-2", "status": "completed"}}});
    assert_eq!(
        gate.observe_event_for_generation("generation-2", &next_started)
            .unwrap(),
        Completion::Ignored
    );
    assert_eq!(
        gate.observe_event_for_generation("generation-2", &completed)
            .unwrap(),
        Completion::Ignored
    );
    assert_eq!(
        gate.observe_event_for_generation("generation-2", &json!({"method": "turn/completed", "params": {"threadId": "other-thread", "turn": {"id": "turn-2", "status": "completed"}}}))
            .unwrap(),
        Completion::Ignored
    );
    assert_eq!(
        gate.observe_event_for_generation("generation-2", &next_completed)
            .unwrap(),
        Completion::Accepted {
            generation: "generation-2".into(),
            turn_id: "turn-2".into(),
            input_sequence: 2,
        }
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
let shutdowns = 0;
const ctx = { sessionManager: manager, shutdown: () => shutdowns++ };
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
await callbacks.get('session_compact')({ type: 'session_compact' }, ctx);
await callbacks.get('agent_settled')({ type: 'agent_settled', aborted: false }, ctx);
assert.deepEqual(messages.slice(-3).map(item => item.event.type), ['input_entry', 'input_entry', 'agent_settled']);
assert.deepEqual(messages.slice(-3, -1).map(item => item.input_sequence), [1, 2]);
assert.equal(callbacks.get('session_before_switch')().cancel, true);
assert.equal(callbacks.get('session_before_fork')().cancel, true);
assert.equal(callbacks.get('session_before_tree')().cancel, true);
assert.equal(callbacks.get('user_bash')().result.exitCode, 1);
await assert.rejects(callbacks.get('session_start')({ type: 'session_start', reason: 'reload' }, ctx));
const failing = new Map();
createFluentPiExtension(async () => { throw new Error('receiver failed'); })
  ({ on: (kind, callback) => failing.set(kind, callback) });
await assert.rejects(failing.get('session_start')({ type: 'session_start', reason: 'startup' }, ctx));
assert.deepEqual(await failing.get('input')({ type: 'input', text: 'late' }, ctx), { action: 'handled' });
assert.ok(shutdowns >= 1);
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

fn installed_pi_runner() -> Option<std::path::PathBuf> {
    let which = std::process::Command::new("sh")
        .args(["-c", "command -v pi"])
        .output()
        .unwrap();
    if !which.status.success() {
        return None;
    }
    let shim = String::from_utf8(which.stdout).unwrap();
    let shim = std::fs::read_to_string(shim.trim()).unwrap();
    let target = shim
        .lines()
        .find_map(|line| line.strip_prefix("# cmd-shim-target="));
    let target = target?;
    let runner = std::path::Path::new(target)
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("core/extensions/runner.js");
    assert!(
        runner.is_file(),
        "installed Pi runner missing: {}",
        runner.display()
    );
    Some(runner)
}

#[test]
fn pi_installed_runner_closes_input_after_receiver_failure() {
    let Some(runner) = installed_pi_runner() else {
        return;
    };
    let output = std::process::Command::new("node")
        .arg("tests/fixtures/pi-installed-runner.mjs")
        .arg(&runner)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn pi_installed_runner_compacts_before_queued_continuation() {
    let Some(runner) = installed_pi_runner() else {
        return;
    };
    let output = std::process::Command::new("node")
        .arg("tests/fixtures/pi-installed-compaction.mjs")
        .arg(&runner)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut gate = fluent::native_agent::pi::PiEventGate::new(
        "compact-generation",
        "/private/task/session.jsonl",
    );
    let mut compacted = false;
    let mut completed = false;
    for line in String::from_utf8(output.stdout).unwrap().lines() {
        let event: serde_json::Value = serde_json::from_str(line).unwrap();
        compacted |= event["event"]["type"] == "session_compact";
        if let Completion::Accepted { input_sequence, .. } =
            gate.observe(&event).unwrap().completion
        {
            assert_eq!(input_sequence, 2);
            completed = true;
        }
    }
    assert!(compacted, "installed Pi did not compact an idle session");
    assert!(
        completed,
        "queued input did not reach durable successful settlement"
    );
}

#[cfg(unix)]
#[test]
fn pi_receiver_waits_for_payload_on_nonblocking_accepted_socket() {
    use fluent::native_agent::pi::{PiEventGate, receive_event};
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    let (mut sender, receiver) = UnixStream::pair().unwrap();
    // Accepted sockets inherit the listener's nonblocking flag on macOS.
    receiver.set_nonblocking(true).unwrap();
    let extension = std::thread::spawn(move || -> std::io::Result<String> {
        std::thread::sleep(Duration::from_millis(50));
        sender
            .write_all(b"{\"generation\":\"generation-1\",\"session_id\":\"assigned-session\",\"session_file\":\"/private/task/session.jsonl\",\"event\":{\"type\":\"session_start\",\"reason\":\"startup\"}}\n")
            ?;
        let mut ack = String::new();
        BufReader::new(sender).read_line(&mut ack)?;
        Ok(ack)
    });
    let evidence = tempfile::NamedTempFile::new().unwrap();
    let mut writer = evidence.reopen().unwrap();
    let mut gate = PiEventGate::new("generation-1", "/private/task/session.jsonl");
    let received = receive_event(receiver, &mut gate, &mut writer);
    let ack = extension.join().unwrap();
    assert!(received.is_ok(), "{received:?}");
    let ack: serde_json::Value = serde_json::from_str(&ack.unwrap()).unwrap();
    assert_eq!(ack, json!({"ok":true,"input_sequence":null}));
    assert_eq!(gate.session_id(), Some("assigned-session"));
    assert_eq!(
        std::fs::read_to_string(evidence.path())
            .unwrap()
            .lines()
            .count(),
        1
    );
}

#[cfg(unix)]
#[test]
fn pi_receiver_retains_large_tool_result_before_acknowledging() {
    use fluent::native_agent::pi::{PiEventGate, receive_event};
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    let (mut sender, receiver) = UnixStream::pair().unwrap();
    sender
        .set_write_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let result = "x".repeat(6 * 1024 * 1024);
    let payload = format!(
        "{}\n",
        json!({
            "generation":"generation-1", "session_id":"assigned-session",
            "session_file":"/private/task/session.jsonl",
            "event":{"type":"tool_execution_end", "result":result}
        })
    );
    let extension = std::thread::spawn(move || -> std::io::Result<String> {
        sender.write_all(payload.as_bytes())?;
        let mut ack = String::new();
        BufReader::new(sender).read_line(&mut ack)?;
        Ok(ack)
    });
    let evidence = tempfile::NamedTempFile::new().unwrap();
    let mut writer = evidence.reopen().unwrap();
    let mut gate = PiEventGate::new("generation-1", "/private/task/session.jsonl");
    gate.observe(&json!({
        "generation":"generation-1", "session_id":"assigned-session",
        "session_file":"/private/task/session.jsonl",
        "event":{"type":"session_start", "reason":"startup"}
    }))
    .unwrap();
    let received = receive_event(receiver, &mut gate, &mut writer);
    let ack = extension.join().unwrap();
    assert!(received.is_ok(), "{received:?}");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&ack.unwrap()).unwrap()["ok"],
        true
    );
    let saved = std::fs::read_to_string(evidence.path()).unwrap();
    assert!(saved.contains(&"x".repeat(6 * 1024 * 1024)));
    let mut final_completion = Completion::Ignored;
    for event in [
        json!({"type":"input","text":"review","streamingBehavior":"followUp"}),
        json!({"type":"input_entry","entry_id":"review-entry"}),
        json!({"type":"agent_before_settle","outcome":"completed"}),
        json!({"type":"agent_settled","aborted":false}),
    ] {
        let (mut client, receiver) = UnixStream::pair().unwrap();
        let mut envelope = json!({
            "generation":"generation-1", "session_id":"assigned-session",
            "session_file":"/private/task/session.jsonl", "event":event
        });
        if event["type"] == "input_entry" {
            envelope["input_sequence"] = json!(1);
        }
        client
            .write_all(format!("{envelope}\n").as_bytes())
            .unwrap();
        final_completion = receive_event(receiver, &mut gate, &mut writer).unwrap();
        let mut reply = String::new();
        BufReader::new(client).read_line(&mut reply).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&reply).unwrap()["ok"],
            true
        );
    }
    assert!(matches!(
        final_completion,
        Completion::Accepted {
            input_sequence: 1,
            ..
        }
    ));
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
        json!({"type":"owner_heartbeat"}),
        json!({"type":"input","text":"first","streamingBehavior":"steer"}),
        json!({"type":"input","text":"second","streamingBehavior":"followUp"}),
        json!({"type":"input_entry","entry_id":"entry-1"}),
        json!({"type":"input_entry","entry_id":"entry-2"}),
        json!({"type":"session_compact"}),
        json!({"type":"session_compact_failed"}),
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
    assert_eq!(observed[2].1, json!(1));
    assert_eq!(observed[3].1, json!(2));
    assert!(matches!(
        observed.last().unwrap().2,
        Completion::Accepted {
            input_sequence: 2,
            ..
        }
    ));
    let saved = std::fs::read_to_string(evidence.path()).unwrap();
    assert_eq!(saved.lines().count(), 10);
    assert!(saved.contains("entry-2"));
    assert!(gate.observe(&json!({"generation":"generation-old","session_id":"assigned-session","session_file":"/private/task/session.jsonl","event":{"type":"agent_settled","aborted":false}})).is_err());
}

#[cfg(unix)]
#[test]
fn pi_receiver_failure_does_not_acknowledge_input() {
    use fluent::native_agent::pi::{PiEventGate, receive_event};
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;

    let evidence = tempfile::NamedTempFile::new().unwrap();
    let mut read_only = std::fs::File::open(evidence.path()).unwrap();
    let mut gate = PiEventGate::new("generation-1", "/private/task/session.jsonl");
    let (mut extension, receiver) = UnixStream::pair().unwrap();
    extension.write_all(b"{\"generation\":\"generation-1\",\"session_id\":\"assigned\",\"session_file\":\"/private/task/session.jsonl\",\"event\":{\"type\":\"session_start\",\"reason\":\"startup\"}}\n").unwrap();
    assert!(receive_event(receiver, &mut gate, &mut read_only).is_err());
    let mut reply = String::new();
    assert_eq!(BufReader::new(extension).read_line(&mut reply).unwrap(), 0);
    assert!(reply.is_empty());
}

#[cfg(unix)]
#[test]
fn pi_receiver_records_rejected_input_at_owned_boundary() {
    use fluent::native_agent::pi::{PiEventGate, receive_event};
    use std::io::Write;
    use std::os::unix::net::UnixStream;

    let evidence = tempfile::NamedTempFile::new().unwrap();
    let mut writer = evidence.reopen().unwrap();
    let mut gate = PiEventGate::new("generation-1", "/private/task/session.jsonl");
    for event in [
        json!({"type":"session_start","reason":"startup"}),
        json!({"type":"input","text":"wrong-session-probe"}),
    ] {
        let (mut extension, receiver) = UnixStream::pair().unwrap();
        extension
            .write_all(
                format!(
                    "{}\n",
                    json!({"generation":"generation-1","session_id":
                        if event["type"] == "input" { "other" } else { "assigned" },
                        "session_file":"/private/task/session.jsonl","event":event})
                )
                .as_bytes(),
            )
            .unwrap();
        if event["type"] == "input" {
            assert!(receive_event(receiver, &mut gate, &mut writer).is_err());
        } else {
            receive_event(receiver, &mut gate, &mut writer).unwrap();
        }
    }
    let records: Vec<serde_json::Value> = std::fs::read_to_string(evidence.path())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(records.iter().any(|record| {
        record["execution_generation"] == "generation-1"
            && record["event"]["type"] == "input_rejected"
            && record["event"]["request"]["text"] == "wrong-session-probe"
    }));
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

#[test]
fn pi_endpoint_override_preserves_effective_model() {
    use fluent::native_agent::pi::select_local_model_config_with_endpoint;
    let source = json!({"providers": {"local-openai": {
        "baseUrl":"http://127.0.0.1:10000/v1", "api":"openai-completions", "apiKey":"original",
        "models":[{"id":"assigned", "reasoning":true, "contextWindow":262144,
            "maxTokens":8192, "supportsDeveloperRole":false,
            "supportsReasoningEffort":false, "maxTokensField":"max_tokens"},
            {"id":"other", "contextWindow":1024}]
    }, "remote":{"models":[{"id":"remote"}]}}});
    let selected = select_local_model_config_with_endpoint(
        &source,
        "assigned",
        "http://127.0.0.1:18989/v1",
        Some("local"),
    )
    .unwrap();
    assert_eq!(
        selected["providers"]["local-openai"]["baseUrl"],
        "http://127.0.0.1:18989/v1"
    );
    assert_eq!(selected["providers"]["local-openai"]["apiKey"], "local");
    assert_eq!(
        selected["providers"]["local-openai"]["models"],
        json!([source["providers"]["local-openai"]["models"][0]])
    );
    assert!(selected["providers"].get("remote").is_none());
    assert!(
        select_local_model_config_with_endpoint(
            &source,
            "missing",
            "http://127.0.0.1:18989/v1",
            None
        )
        .is_err()
    );
    assert!(
        select_local_model_config_with_endpoint(
            &source,
            "assigned",
            "https://remote.invalid/v1",
            None
        )
        .is_err()
    );
    assert!(
        select_local_model_config_with_endpoint(
            &source,
            "assigned",
            "http://127.0.0.1:80@remote.invalid/v1",
            None,
        )
        .is_err()
    );
    let incompatible = json!({"providers":{"local-openai":{
        "baseUrl":"http://127.0.0.1:10000/v1", "api":"unsupported",
        "models":[source["providers"]["local-openai"]["models"][0].clone()]
    }}});
    assert!(
        select_local_model_config_with_endpoint(
            &incompatible,
            "assigned",
            "http://127.0.0.1:18989/v1",
            None
        )
        .is_err()
    );
}

#[cfg(feature = "test-support")]
#[test]
fn attached_operator_does_not_hold_workflow_progression() {
    if std::env::var("FLUENT_TEST_REAL_NATIVE_AGENT_HOST").as_deref() == Ok("1") {
        host_native_attached_attempt_progression();
        return;
    }
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

    if std::env::var("FLUENT_TEST_REAL_NATIVE_AGENT_HOST").as_deref() == Ok("1") {
        run_host_native_task_continuation("writer-retry");
        return;
    }

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
    assert_eq!(attempt_record["schema_version"], 14);
    assert_eq!(attempt_record["terminal_mode"], "native");
    let mut loaded = store.read_work_item_snapshot("work-1").unwrap();
    reopen_attempt(&mut loaded.attempts[0]);
    assert_eq!(loaded.attempts[0].terminal_mode, TerminalMode::Native);

    // A correction round creates a new Writer Task from the stored Attempt.
    // The authorized host fixture also exercises the resulting Task launch.
    store
        .mutate_work_item("work-1", |item| {
            let task = &mut item.attempts[0].tasks[0];
            fluent::work_model::set_task_terminal(task, fluent::work_model::TaskStatus::Complete);
            let workspace = &task.workspace_access.writes[0];
            task.output = Some(fluent::work_model::TaskOutput {
                workspace_id: workspace.id.clone(),
                workspace_path: workspace.path.clone(),
                source_branch: "main".into(),
                base_commit: None,
                commit: "first-candidate".into(),
                no_change: None,
                learner_canonicalization: None,
            });
            item.add_next_write_round("attempt-1", vec![])
        })
        .unwrap();
    let retried = store.read_work_item_snapshot("work-1").unwrap();
    assert_eq!(retried.attempts[0].terminal_mode, TerminalMode::Native);
    assert_eq!(retried.attempts[0].tasks[1].id, "attempt-1-write-2");
    assert_eq!(
        retried.attempts[0].tasks[1].status,
        fluent::work_model::TaskStatus::Planned
    );

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
    assert!(!remote_args.contains(&"--dangerously-bypass-approvals-and-sandbox".to_string()));
    assert_eq!(remote_command.get_program(), OsStr::new("sandbox-exec"));
    assert!(
        remote_args
            .windows(2)
            .any(|args| args == ["-f", "(version 1) (allow default)"])
    );
    assert!(
        remote_args
            .windows(2)
            .any(|args| args == ["--model", "gpt-assigned"])
    );
    assert!(
        remote_args
            .windows(2)
            .any(|args| args == ["-c", "model_reasoning_effort=high"])
    );
    assert!(
        remote_args
            .windows(2)
            .any(|args| args == ["--disable", "hooks"])
    );
    assert!(
        remote_command
            .get_envs()
            .any(|(name, value)| name == OsStr::new("CODEX_HOME")
                && value == Some(private_home.path().as_os_str()))
    );
    assert_eq!(remote_command.get_current_dir(), Some(project.path()));
    assert!(args.contains(&"--dangerously-bypass-approvals-and-sandbox".to_string()));
    assert!(resumed_args.contains(&"--dangerously-bypass-approvals-and-sandbox".to_string()));

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
        &root.path().join("generation"),
        &root.path().join("client-exit"),
        "assigned Task prompt",
    )
    .unwrap();
    assert!(script.contains("'app-server' '--listen'"));
    assert!(script.contains("--remote"));
    assert!(script.contains("'resume' \"$assigned_thread\""));
    assert!(!script.contains("--dangerously-bypass-approvals-and-sandbox"));
    assert!(script.contains("--model"));
    assert!(script.contains("gpt-assigned"));
    assert!(script.contains("model_reasoning_effort=high"));
    assert!(script.contains("--disable"));
    assert!(script.contains("hooks"));
    assert!(script.contains("assigned Task prompt"));
    assert!(script.contains("assigned_generation=$(cat"));
    assert!(script.contains("client_status=$?"));
    assert!(script.contains("$assigned_generation"));
    assert!(script.contains("$assigned_thread"));
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

#[test]
fn native_claude_hooks_account_for_reused_prompt_ids() {
    use fluent::native_agent::claude::ClaudeHookGate;

    let mut gate = ClaudeHookGate::new("generation-1", Some("assigned-session"));
    gate.observe("generation-1", &json!({"hook_event_name":"SessionStart","session_id":"assigned-session","source":"startup"})).unwrap();
    let first = json!({"hook_event_name":"UserPromptSubmit","session_id":"assigned-session","prompt_id":"same-prompt","prompt":"first attached input"});
    let second = json!({"hook_event_name":"UserPromptSubmit","session_id":"assigned-session","prompt_id":"same-prompt","prompt":"second attached input"});
    gate.observe("generation-1", &first).unwrap();
    assert_eq!(gate.event_sequence(), 1);
    gate.observe("generation-1", &second).unwrap();
    assert_eq!(gate.last_sequence(), 2);
    assert_eq!(gate.event_sequence(), 2);

    let stop = json!({"hook_event_name":"Stop","session_id":"assigned-session","prompt_id":"same-prompt","last_assistant_message":"done","background_tasks":[],"session_crons":[]});
    assert_eq!(
        gate.observe("generation-1", &stop).unwrap(),
        Completion::Accepted {
            generation: "generation-1".into(),
            turn_id: "same-prompt".into(),
            input_sequence: 2,
        }
    );
    assert_eq!(gate.event_sequence(), 2);
    assert_eq!(
        gate.observe("generation-1", &stop).unwrap(),
        Completion::Ignored
    );
    assert_eq!(gate.event_sequence(), 0);
}

#[test]
fn native_claude_settles_coalesced_attached_inputs() {
    use fluent::native_agent::claude::ClaudeHookGate;

    let mut gate = ClaudeHookGate::new("generation-1", Some("assigned-session"));
    gate.observe("generation-1", &json!({"hook_event_name":"SessionStart","session_id":"assigned-session","source":"startup"})).unwrap();
    for prompt in ["initial", "attached steering", "attached follow-up"] {
        gate.observe("generation-1", &json!({"hook_event_name":"UserPromptSubmit","session_id":"assigned-session","prompt_id":"same-prompt","prompt":prompt})).unwrap();
    }
    let stop = json!({"hook_event_name":"Stop","session_id":"assigned-session","prompt_id":"same-prompt","last_assistant_message":"final answer","background_tasks":[],"session_crons":[]});
    assert_eq!(
        gate.observe("generation-1", &stop).unwrap(),
        Completion::Accepted {
            generation: "generation-1".into(),
            turn_id: "same-prompt".into(),
            input_sequence: 3,
        }
    );
    assert!(gate.observe("generation-1", &json!({"hook_event_name":"UserPromptSubmit","session_id":"assigned-session","prompt_id":"same-prompt","prompt":"too late"})).is_err());
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
    let evidence_writer = evidence.reopen().unwrap();
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
            evidence_writer,
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
    let evidence: Vec<serde_json::Value> = std::fs::read_to_string(&evidence_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(evidence.len(), 3);
    assert_eq!(evidence[0]["event"]["method"], "turn/start");
    assert_eq!(evidence[0]["input_sequence"], 1);
    assert_eq!(
        evidence[1]["event"],
        serde_json::from_str::<serde_json::Value>(started).unwrap()
    );
    assert_eq!(
        evidence[2]["event"],
        serde_json::from_str::<serde_json::Value>(finished).unwrap()
    );

    // Completion closes this invocation. A later turn addressed to the same
    // conversation must not cross the relay into the provider.
    tui.write_all(b"{\"id\":3,\"method\":\"turn/start\",\"params\":{\"threadId\":\"assigned-thread\",\"input\":[{\"text\":\"closed-probe\"}]}}\n").unwrap();
    line.clear();
    assert_eq!(provider_lines.read_line(&mut line).unwrap(), 0);
    let error = relay.join().unwrap().unwrap_err();
    assert!(error.to_string().contains("input is closed"));
    let records: Vec<serde_json::Value> = std::fs::read_to_string(evidence_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(records.iter().any(|record| {
        record["execution_generation"] == "generation-1"
            && record["event"]["type"] == "input_rejected"
            && record["event"]["request"]
                .to_string()
                .contains("closed-probe")
    }));
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
        let steering: serde_json::Value =
            serde_json::from_str(&socket.read().unwrap().into_text().unwrap()).unwrap();
        assert_eq!(steering["method"], "turn/steer");
        assert_eq!(steering["params"]["threadId"], "assigned-thread");
        for event in [
            json!({"method":"turn/started","params":{"threadId":"assigned-thread","turn":{"id":"turn-1"}}}),
            json!({"id":3,"result":{}}),
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
        json!({"id":2,"method":"turn/start","params":{"threadId":"assigned-thread","input":[{"text":"first attached input"}]}})
            .to_string(),
    ))
    .unwrap();
    tui.send(Message::Text(
        json!({"id":3,"method":"turn/steer","params":{"threadId":"assigned-thread","input":[{"text":"second attached input"}]}})
            .to_string(),
    ))
    .unwrap();
    for method in ["turn/started", "response", "turn/completed"] {
        let event: serde_json::Value =
            serde_json::from_str(&tui.read().unwrap().into_text().unwrap()).unwrap();
        if method == "response" {
            assert_eq!(event["id"], 3);
            continue;
        }
        assert_eq!(event["method"], method);
    }
    assert_eq!(
        received.recv_timeout(Duration::from_secs(2)).unwrap(),
        Completion::Accepted {
            generation: "generation-1".into(),
            turn_id: "turn-1".into(),
            input_sequence: 2,
        }
    );
    tui.send(Message::Text(
        json!({"id":4,"method":"turn/start","params":{"threadId":"assigned-thread","input":[]}})
            .to_string(),
    ))
    .unwrap();
    let rejection: serde_json::Value =
        serde_json::from_str(&tui.read().unwrap().into_text().unwrap()).unwrap();
    assert_eq!(rejection["id"], 4);
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
    let inputs: Vec<_> = evidence
        .iter()
        .filter(|record| native_input_event(record))
        .collect();
    assert_eq!(inputs.len(), 2);
    assert_eq!(inputs[0]["input_sequence"], 1);
    assert_eq!(inputs[1]["input_sequence"], 2);
    for (record, marker) in inputs
        .into_iter()
        .zip(["first attached input", "second attached input"])
    {
        assert_eq!(record["execution_generation"], "generation-1");
        assert_eq!(record["assigned_conversation_id"], "assigned-thread");
        assert_eq!(record["conversation_id"], "assigned-thread");
        assert!(record["event"].to_string().contains(marker));
    }
    assert!(evidence.iter().any(
        |record| record["event"]["method"] == "turn/completed" && record["input_sequence"] == 2
    ));
    let _ = tui.get_ref().shutdown(Shutdown::Both);
    provider.join().unwrap();
    let _ = relay.join().unwrap();
}

#[cfg(unix)]
#[test]
fn codex_relay_bounds_plugin_responses_without_losing_turn_evidence() {
    codex_relay_preserves_responses_and_turn_evidence("plugin/list", "plugins", 512 * 1024, 4);
}

#[cfg(unix)]
#[test]
fn codex_relay_delivers_oversized_app_response_without_losing_turn_evidence() {
    codex_relay_preserves_responses_and_turn_evidence("app/read", "apps", 15_897_799, 1);
}

#[cfg(unix)]
fn codex_relay_preserves_responses_and_turn_evidence(
    method: &'static str,
    catalog: &'static str,
    response_bytes: usize,
    response_count: u64,
) {
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
    let evidence_file = evidence.reopen().unwrap();
    let (release_tx, release_rx) = mpsc::channel();
    let (completed_tx, completed_rx) = mpsc::channel();
    let provider = std::thread::spawn(move || {
        let mut socket = tungstenite::accept(provider_stream).unwrap();
        socket
            .get_ref()
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        for id in 1..=response_count {
            let request: serde_json::Value =
                serde_json::from_str(&socket.read().unwrap().into_text().unwrap()).unwrap();
            assert_eq!(request["method"], method);
            assert_eq!(request["id"], id);
            socket
                .send(Message::Text(
                    json!({"id":id,"result":{catalog:[{"description":"x".repeat(response_bytes)}]}})
                        .to_string(),
                ))
                .unwrap();
        }
        let turn: serde_json::Value =
            serde_json::from_str(&socket.read().unwrap().into_text().unwrap()).unwrap();
        assert_eq!(turn["method"], "turn/start");
        for event in [
            json!({"method":"turn/started","params":{"threadId":"assigned-thread","turn":{"id":"turn-1"}}}),
            json!({"method":"turn/completed","params":{"threadId":"assigned-thread","turn":{"id":"turn-1","status":"completed"}}}),
        ] {
            socket.send(Message::Text(event.to_string())).unwrap();
        }
        release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    });
    let relay = std::thread::spawn(move || {
        let client = tungstenite::accept(relay_client).unwrap();
        let (server, _) = tungstenite::client("ws://localhost/", relay_server).unwrap();
        relay_codex_websockets(
            client,
            server,
            "generation-1",
            "assigned-thread",
            Arc::new(NativeAdmission::new()),
            evidence_file,
            |completion| completed_tx.send(completion).map_err(anyhow::Error::new),
        )
    });
    let (mut tui, _) = tungstenite::client("ws://localhost/", tui_stream).unwrap();
    tui.get_ref()
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    for id in 1..=response_count {
        tui.send(Message::Text(
            json!({"id":id,"method":method,"params":{}}).to_string(),
        ))
        .unwrap();
        let response: serde_json::Value =
            serde_json::from_str(&tui.read().unwrap().into_text().unwrap()).unwrap();
        assert_eq!(response["id"], id);
        let description = response["result"][catalog][0]["description"]
            .as_str()
            .unwrap();
        assert_eq!(description.len(), response_bytes);
        assert!(description.bytes().all(|byte| byte == b'x'));
    }
    tui.send(Message::Text(
        json!({"id":5,"method":"turn/start","params":{"threadId":"assigned-thread","input":[{"text":"review candidate"}]}})
            .to_string(),
    ))
    .unwrap();
    for method in ["turn/started", "turn/completed"] {
        let event: serde_json::Value =
            serde_json::from_str(&tui.read().unwrap().into_text().unwrap()).unwrap();
        assert_eq!(event["method"], method);
    }
    assert_eq!(
        completed_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        Completion::Accepted {
            generation: "generation-1".into(),
            turn_id: "turn-1".into(),
            input_sequence: 1,
        }
    );
    release_tx.send(()).unwrap();
    let _ = tui.get_ref().shutdown(Shutdown::Both);
    provider.join().unwrap();
    let _ = relay.join().unwrap();
    let transcript = std::fs::read_to_string(evidence_path).unwrap();
    assert!(
        transcript.len() < 64 * 1024,
        "opaque responses filled canonical evidence"
    );
    let records: Vec<serde_json::Value> = transcript
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(records.iter().any(|record| {
        native_input_event(record)
            && record["execution_generation"] == "generation-1"
            && record["event"].to_string().contains("review candidate")
    }));
    assert!(records.iter().any(|record| {
        record["event"]["method"] == "turn/completed" && record["input_sequence"] == 1
    }));
}

#[cfg(unix)]
#[test]
fn codex_relay_delivers_overlapping_app_responses_and_turn_events() {
    use fluent::native_agent::codex::relay_codex_websockets_with_trace;
    use std::net::Shutdown;
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;
    use std::sync::{Arc, mpsc};
    use std::time::Duration;
    use tungstenite::Message;

    let response_sizes = [(0, 512 * 1024), (1, 15_897_799), (2, 15_897_799)];
    // Construct the large payloads before opening sockets. Fixture CPU work
    // must not consume the production display's bounded drain interval.
    let responses = response_sizes
        .map(|(id, bytes)| json!({"id":id,"result":{"data":"x".repeat(bytes)}}).to_string());
    let (tui_stream, relay_client) = UnixStream::pair().unwrap();
    let (relay_server, provider_stream) = UnixStream::pair().unwrap();
    // Keep the first ordinary response in flight while both app/read
    // responses and turn events arrive. No timing-dependent sleep or mock
    // queue is needed: this peer waits for provider completion before reading.
    let buffer_size: libc::c_int = 4096;
    assert_eq!(
        unsafe {
            libc::setsockopt(
                relay_client.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_SNDBUF,
                (&buffer_size as *const libc::c_int).cast(),
                std::mem::size_of_val(&buffer_size) as libc::socklen_t,
            )
        },
        0
    );
    let evidence = tempfile::NamedTempFile::new().unwrap();
    let evidence_file = evidence.reopen().unwrap();
    let trace = tempfile::NamedTempFile::new().unwrap();
    let trace_file = trace.reopen().unwrap();
    let (release_tx, release_rx) = mpsc::channel();
    let (completed_tx, completed_rx) = mpsc::channel();
    let provider = std::thread::spawn(move || {
        let mut socket = tungstenite::accept(provider_stream).unwrap();
        socket
            .get_ref()
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        for (id, method) in [
            (0, "plugin/list"),
            (1, "app/read"),
            (2, "app/read"),
            (3, "turn/start"),
        ] {
            let request: serde_json::Value =
                serde_json::from_str(&socket.read().unwrap().into_text().unwrap()).unwrap();
            assert_eq!(request["id"], id);
            assert_eq!(request["method"], method);
        }
        for response in responses {
            socket.send(Message::Text(response)).unwrap();
        }
        socket.send(Message::Text(json!({"method":"turn/started","params":{"threadId":"assigned-thread","turn":{"id":"turn-1"}}}).to_string())).unwrap();
        for sequence in 0..8 {
            socket.send(Message::Text(json!({"method":"item/updated","params":{"threadId":"assigned-thread","turnId":"turn-1","sequence":sequence}}).to_string())).unwrap();
        }
        socket.send(Message::Text(json!({"method":"turn/completed","params":{"threadId":"assigned-thread","turn":{"id":"turn-1","status":"completed"}}}).to_string())).unwrap();
        release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    });
    let relay = std::thread::spawn(move || {
        let client = tungstenite::accept(relay_client).unwrap();
        let (server, _) = tungstenite::client("ws://localhost/", relay_server).unwrap();
        relay_codex_websockets_with_trace(
            client,
            server,
            "generation-1",
            "assigned-thread",
            Arc::new(NativeAdmission::new()),
            evidence_file,
            |completion| completed_tx.send(completion).map_err(anyhow::Error::new),
            trace_file,
        )
    });
    let (mut tui, _) = tungstenite::client("ws://localhost/", tui_stream).unwrap();
    tui.get_ref()
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    for (id, method) in [
        (0, "plugin/list"),
        (1, "app/read"),
        (2, "app/read"),
        (3, "turn/start"),
    ] {
        tui.send(Message::Text(
            json!({"id":id,"method":method,"params":{"threadId":"assigned-thread","input":[]}})
                .to_string(),
        ))
        .unwrap();
    }
    assert_eq!(
        completed_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        Completion::Accepted {
            generation: "generation-1".into(),
            turn_id: "turn-1".into(),
            input_sequence: 1,
        }
    );
    // Provider evidence settles even though the display has not consumed
    // any responses. Opaque catalogs never become canonical turn evidence.
    let transcript = std::fs::read_to_string(evidence.path()).unwrap();
    let trace_before_read = std::fs::read_to_string(trace.path()).unwrap();
    // Drain the wire before parsing and comparing large JSON payloads. Keep
    // the evidence snapshot from before the display consumed any frame.
    let mut received = (0..response_sizes.len() + 10)
        .map(|_| {
            tui.read()
                .unwrap_or_else(|error| {
                    panic!(
                        "{error}; relay trace: {}",
                        std::fs::read_to_string(trace.path()).unwrap()
                    )
                })
                .into_text()
                .unwrap()
        })
        .collect::<Vec<_>>()
        .into_iter();
    assert!(transcript.len() < 64 * 1024);
    let records: Vec<serde_json::Value> = transcript
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(records.len(), 11);
    assert!(
        records
            .iter()
            .all(|record| record["execution_generation"] == "generation-1"
                && record["input_sequence"] == 1)
    );
    assert_eq!(records.last().unwrap()["event"]["method"], "turn/completed");
    assert!(!trace_before_read.contains("display-detached"));
    for (id, bytes) in response_sizes {
        let response: serde_json::Value = serde_json::from_str(&received.next().unwrap()).unwrap();
        assert_eq!(response["id"], id);
        assert_eq!(response["result"]["data"], "x".repeat(bytes));
    }
    let mut read_event =
        || -> serde_json::Value { serde_json::from_str(&received.next().unwrap()).unwrap() };
    assert_eq!(read_event()["method"], "turn/started");
    for sequence in 0..8 {
        let event = read_event();
        assert_eq!(event["method"], "item/updated");
        assert_eq!(event["params"]["sequence"], sequence);
    }
    assert_eq!(read_event()["method"], "turn/completed");
    release_tx.send(()).unwrap();
    let _ = tui.get_ref().shutdown(Shutdown::Both);
    provider.join().unwrap();
    let _ = relay.join().unwrap();
}

#[cfg(unix)]
#[test]
fn codex_relay_records_which_peer_exits_before_first_turn() {
    use fluent::native_agent::codex::relay_codex_websockets_with_trace;
    use std::os::unix::net::UnixStream;
    use std::sync::Arc;
    use std::time::Duration;
    use tungstenite::Message;

    let (tui_stream, relay_client) = UnixStream::pair().unwrap();
    let (relay_server, provider_stream) = UnixStream::pair().unwrap();
    let trace_dir = tempfile::tempdir().unwrap();
    let trace_path = trace_dir.path().join("relay.jsonl");
    let trace = std::fs::File::create(&trace_path).unwrap();
    let provider = std::thread::spawn(move || {
        let mut socket = tungstenite::accept(provider_stream).unwrap();
        let _ = socket.read();
        std::thread::sleep(Duration::from_millis(300));
    });
    let relay = std::thread::spawn(move || {
        let client = tungstenite::accept(relay_client).unwrap();
        let (server, _) = tungstenite::client("ws://localhost/", relay_server).unwrap();
        relay_codex_websockets_with_trace(
            client,
            server,
            "generation-1",
            "assigned-thread",
            Arc::new(NativeAdmission::new()),
            std::io::sink(),
            |_| Ok(()),
            trace,
        )
    });
    let (mut tui, _) = tungstenite::client("ws://localhost/", tui_stream).unwrap();
    tui.send(Message::Text(
        json!({"id":1,"method":"thread/resume","params":{"threadId":"assigned-thread","secret":"private-token"}})
            .to_string(),
    ))
    .unwrap();
    std::thread::sleep(Duration::from_millis(100));
    tui.close(None).unwrap();
    drop(tui);
    let _ = relay.join().unwrap();
    provider.join().unwrap();
    let diagnostic = std::fs::read_to_string(trace_path).unwrap();
    assert!(!diagnostic.contains("private-token"));
    let records: Vec<serde_json::Value> = diagnostic
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(
        records
            .iter()
            .any(|record| record["direction"] == "tui-to-app"
                && record["outcome"] == "closed"
                && record["input_sequence"] == 0)
    );
    assert!(
        records
            .iter()
            .all(|record| record["execution_generation"] == "generation-1"
                && record["assigned_conversation_id"] == "assigned-thread")
    );
}

#[cfg(unix)]
#[test]
fn codex_relay_records_app_server_exit_before_first_turn() {
    use fluent::native_agent::codex::relay_codex_websockets_with_trace;
    use std::os::unix::net::UnixStream;
    use std::sync::Arc;
    use tungstenite::Message;

    let (tui_stream, relay_client) = UnixStream::pair().unwrap();
    let (relay_server, provider_stream) = UnixStream::pair().unwrap();
    let trace_dir = tempfile::tempdir().unwrap();
    let trace_path = trace_dir.path().join("relay.jsonl");
    let trace = std::fs::File::create(&trace_path).unwrap();
    let provider = std::thread::spawn(move || {
        let mut socket = tungstenite::accept(provider_stream).unwrap();
        let _ = socket.read();
        socket.close(None).unwrap();
    });
    let relay = std::thread::spawn(move || {
        let client = tungstenite::accept(relay_client).unwrap();
        let (server, _) = tungstenite::client("ws://localhost/", relay_server).unwrap();
        relay_codex_websockets_with_trace(
            client,
            server,
            "generation-2",
            "assigned-thread",
            Arc::new(NativeAdmission::new()),
            std::io::sink(),
            |_| Ok(()),
            trace,
        )
    });
    let (mut tui, _) = tungstenite::client("ws://localhost/", tui_stream).unwrap();
    tui.send(Message::Text(
        json!({"id":1,"method":"thread/resume","params":{"threadId":"assigned-thread"}})
            .to_string(),
    ))
    .unwrap();
    let _ = relay.join().unwrap();
    provider.join().unwrap();
    let records: Vec<serde_json::Value> = std::fs::read_to_string(trace_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let first_end = records
        .iter()
        .find(|record| matches!(record["outcome"].as_str(), Some("closed" | "read-error")))
        .unwrap();
    assert_eq!(first_end["direction"], "app-to-tui");
    assert_eq!(first_end["input_sequence"], 0);
}

#[cfg(unix)]
#[test]
fn codex_relay_reports_broken_pipe_without_payloads() {
    assert_codex_display_transport_diagnostic(false);
}

#[cfg(unix)]
#[test]
fn codex_relay_reports_display_drain_timeout_without_payloads() {
    assert_codex_display_transport_diagnostic(true);
}

#[cfg(unix)]
fn assert_codex_display_transport_diagnostic(stall: bool) {
    use fluent::native_agent::codex::relay_codex_websockets_with_trace;
    use std::io::{self, Write};
    use std::net::Shutdown;
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;
    use std::sync::{Arc, mpsc};
    use std::time::Duration;
    use tungstenite::Message;

    struct TraceEvents {
        bytes: Vec<u8>,
        events: mpsc::Sender<serde_json::Value>,
    }

    impl Write for TraceEvents {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            let event = serde_json::from_slice(&self.bytes)?;
            self.bytes.clear();
            self.events.send(event).unwrap();
            Ok(())
        }
    }

    let (tui_stream, relay_client) = UnixStream::pair().unwrap();
    let (relay_server, provider_stream) = UnixStream::pair().unwrap();
    let buffer_size: libc::c_int = 4096;
    assert_eq!(
        unsafe {
            libc::setsockopt(
                relay_client.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_SNDBUF,
                (&buffer_size as *const libc::c_int).cast(),
                std::mem::size_of_val(&buffer_size) as libc::socklen_t,
            )
        },
        0
    );
    let (events_tx, events_rx) = mpsc::channel();
    let (completion_tx, completion_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let provider = std::thread::spawn(move || {
        let mut socket = tungstenite::accept(provider_stream).unwrap();
        socket.read().unwrap();
        socket.send(Message::Text(json!({"id":1,"result":{"turn":{"id":"turn-1"},"private":"private-token".repeat(65536)}}).to_string())).unwrap();
        socket.send(Message::Text(json!({"method":"turn/started","params":{"threadId":"assigned-thread","turn":{"id":"turn-1"}}}).to_string())).unwrap();
        socket.send(Message::Text(json!({"method":"turn/completed","params":{"threadId":"assigned-thread","turn":{"id":"turn-1","status":"completed"}}}).to_string())).unwrap();
        release_rx.recv_timeout(Duration::from_secs(15)).unwrap();
    });
    let relay = std::thread::spawn(move || {
        let client = tungstenite::accept(relay_client).unwrap();
        if !stall {
            // Keep the input side open so only the display send fails.
            client.get_ref().shutdown(Shutdown::Write).unwrap();
        }
        let (server, _) = tungstenite::client("ws://localhost/", relay_server).unwrap();
        relay_codex_websockets_with_trace(
            client,
            server,
            "generation-1",
            "assigned-thread",
            Arc::new(NativeAdmission::new()),
            io::sink(),
            |completion| completion_tx.send(completion).map_err(anyhow::Error::new),
            TraceEvents {
                bytes: Vec::new(),
                events: events_tx,
            },
        )
    });
    let (mut tui, _) = tungstenite::client("ws://localhost/", tui_stream).unwrap();
    tui.send(Message::Text(
        json!({"id":1,"method":"turn/start","params":{"threadId":"assigned-thread","input":[]}})
            .to_string(),
    ))
    .unwrap();
    // Leave the TUI unread while canonical completion proceeds independently.
    let completion = completion_rx.recv_timeout(Duration::from_secs(5));
    let failure = loop {
        let event = events_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(!event.to_string().contains("private-token"));
        if event["direction"] == "app-to-tui" && event["outcome"] == "send-error" {
            break event;
        }
    };
    release_tx.send(()).unwrap();
    provider.join().unwrap();
    let _ = relay.join().unwrap();
    assert!(matches!(
        completion.unwrap(),
        Completion::Accepted {
            input_sequence: 1,
            ..
        }
    ));
    assert_eq!(failure["transport_error"]["kind"], "io", "{failure}");
    if stall {
        assert!(
            matches!(
                failure["transport_error"]["io_kind"].as_str(),
                Some("WouldBlock" | "TimedOut")
            ),
            "{failure}"
        );
    } else {
        assert_eq!(failure["transport_error"]["io_kind"], "BrokenPipe");
        assert_eq!(failure["transport_error"]["raw_os_error"], libc::EPIPE);
    }
}

#[cfg(unix)]
#[test]
fn codex_websocket_backpressure_recovers_required_frame_once() {
    use fluent::native_agent::codex::relay_codex_websockets_with_trace;
    use std::io::{self, Write};
    use std::net::Shutdown;
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;
    use std::sync::{Arc, mpsc};
    use std::time::Duration;
    use tungstenite::Message;

    let (tui_stream, relay_client) = UnixStream::pair().unwrap();
    let (relay_server, provider_stream) = UnixStream::pair().unwrap();
    // Make a single large response exceed the socket buffer while the TUI
    // pauses. The handshake completes before either peer stops reading.
    let buffer_size: libc::c_int = 4096;
    let configured = unsafe {
        libc::setsockopt(
            relay_client.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_SNDBUF,
            (&buffer_size as *const libc::c_int).cast(),
            std::mem::size_of_val(&buffer_size) as libc::socklen_t,
        )
    };
    assert_eq!(configured, 0);

    struct SignalResponseTrace {
        bytes: Vec<u8>,
        signal: Option<mpsc::Sender<()>>,
    }

    impl Write for SignalResponseTrace {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            let event: serde_json::Value = serde_json::from_slice(&self.bytes)?;
            self.bytes.clear();
            if event["direction"] == "app-to-tui"
                && event["outcome"] == "received"
                && event["method"] == "<other>"
                && let Some(signal) = self.signal.take()
            {
                signal.send(()).unwrap();
            }
            Ok(())
        }
    }

    let (response_tx, response_rx) = mpsc::channel();
    let provider = std::thread::spawn(move || {
        let mut socket = tungstenite::accept(provider_stream).unwrap();
        socket
            .get_ref()
            .set_read_timeout(Some(Duration::from_secs(4)))
            .unwrap();
        let resume: serde_json::Value =
            serde_json::from_str(&socket.read().unwrap().into_text().unwrap()).unwrap();
        assert_eq!(resume["method"], "thread/resume");
        assert_eq!(resume["params"]["threadId"], "assigned-thread");
        socket
            .send(Message::Text(
                json!({"id":1,"result":{"data":"x".repeat(8 * 1024 * 1024)}}).to_string(),
            ))
            .unwrap();
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
        assert!(socket.read().is_err(), "unexpected later TUI input");
    });
    let (completed_tx, completed_rx) = mpsc::channel();
    let relay = std::thread::spawn(move || {
        let client = tungstenite::accept(relay_client).unwrap();
        let (server, _) = tungstenite::client("ws://localhost/", relay_server).unwrap();
        relay_codex_websockets_with_trace(
            client,
            server,
            "generation-1",
            "assigned-thread",
            Arc::new(NativeAdmission::new()),
            std::io::sink(),
            move |completion| completed_tx.send(completion).map_err(anyhow::Error::new),
            SignalResponseTrace {
                bytes: Vec::new(),
                signal: Some(response_tx),
            },
        )
    });
    let (mut tui, _) = tungstenite::client("ws://localhost/", tui_stream).unwrap();
    tui.get_ref()
        .set_read_timeout(Some(Duration::from_secs(4)))
        .unwrap();
    tui.send(Message::Text(
        json!({"id":1,"method":"thread/resume","params":{"threadId":"assigned-thread"}})
            .to_string(),
    ))
    .unwrap();
    // The relay received the complete response and is about to enqueue it
    // for display; pause this peer past its socket write timeout.
    response_rx.recv_timeout(Duration::from_secs(4)).unwrap();
    std::thread::sleep(Duration::from_millis(700));
    let response: serde_json::Value =
        serde_json::from_str(&tui.read().unwrap().into_text().unwrap()).unwrap();
    assert_eq!(response["id"], 1);
    assert_eq!(
        response["result"]["data"].as_str().unwrap().len(),
        8 * 1024 * 1024
    );
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
        completed_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
        Completion::Accepted {
            generation: "generation-1".into(),
            turn_id: "turn-1".into(),
            input_sequence: 1,
        }
    );
    tui.get_ref()
        .set_read_timeout(Some(Duration::from_millis(400)))
        .unwrap();
    assert!(tui.read().is_err(), "required frame was delivered twice");
    let _ = tui.get_ref().shutdown(Shutdown::Both);
    provider.join().unwrap();
    // Closing this test peer can reset the relay's concurrent read.
    let _ = relay.join().unwrap();
}

#[cfg(unix)]
#[test]
fn codex_websocket_backpressure_preserves_evidence_and_completion() {
    use fluent::native_agent::codex::relay_codex_websockets_with_trace;
    use std::os::unix::net::UnixStream;
    use std::sync::{Arc, mpsc};
    use std::time::Duration;
    use tungstenite::Message;

    let (tui_stream, relay_client) = UnixStream::pair().unwrap();
    let (relay_server, provider_stream) = UnixStream::pair().unwrap();
    let directory = tempfile::tempdir().unwrap();
    let evidence_path = directory.path().join("evidence.jsonl");
    let evidence = std::fs::File::create(&evidence_path).unwrap();
    let trace_path = directory.path().join("relay-trace.jsonl");
    let trace = std::fs::File::create(&trace_path).unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let (completion_tx, completion_rx) = mpsc::channel();
    let relay = std::thread::spawn(move || {
        let client = tungstenite::accept(relay_client).unwrap();
        let (server, _) = tungstenite::client("ws://localhost/", relay_server).unwrap();
        let result = relay_codex_websockets_with_trace(
            client,
            server,
            "generation-1",
            "assigned-thread",
            Arc::new(NativeAdmission::new()),
            evidence,
            |completion| completion_tx.send(completion).map_err(anyhow::Error::new),
            trace,
        );
        done_tx
            .send(result.map_err(|error| format!("{error:#}")))
            .unwrap();
    });
    let provider = std::thread::spawn(move || {
        let mut socket = tungstenite::accept(provider_stream).unwrap();
        let request = socket.read().unwrap().into_text().unwrap();
        assert!(request.contains("turn/start"));
        socket.send(Message::Text(json!({"method":"turn/started","params":{"threadId":"assigned-thread","turn":{"id":"turn-1"}}}).to_string())).unwrap();
        let frame = json!({"method":"item/updated","params":{"threadId":"assigned-thread","turnId":"turn-1","data":"x".repeat(256 * 1024)}}).to_string();
        for _ in 0..96 {
            socket.send(Message::Text(frame.clone())).unwrap();
        }
        socket.send(Message::Text(json!({"method":"turn/completed","params":{"threadId":"assigned-thread","turn":{"id":"turn-1","status":"completed"}}}).to_string())).unwrap();
        socket.close(None).unwrap();
    });
    let (mut tui, _) = tungstenite::client("ws://localhost/", tui_stream).unwrap();
    tui.send(Message::Text(
        json!({"id":1,"method":"turn/start","params":{"threadId":"assigned-thread","input":[]}})
            .to_string(),
    ))
    .unwrap();
    tui.get_ref()
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    // Keep the TUI socket open without reading app-server frames.
    assert!(matches!(
        completion_rx.recv_timeout(Duration::from_secs(10)).unwrap(),
        Completion::Accepted { .. }
    ));
    let result = done_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("relay did not settle after provider completion");
    assert!(
        result.is_ok(),
        "display backpressure failed the Task: {result:?}"
    );
    assert_eq!(
        std::fs::read_to_string(evidence_path)
            .unwrap()
            .lines()
            .count(),
        99
    );
    let diagnostics = std::fs::read_to_string(trace_path).unwrap();
    let records: Vec<serde_json::Value> = diagnostics
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let detached = records
        .iter()
        .find(|record| record["outcome"] == "display-detached")
        .expect("display overflow must retain its rejection reason");
    let rejection = &detached["display_rejection"];
    assert_eq!(rejection["reason"], "ordinary-byte-limit");
    let queued = rejection["queued_ordinary_bytes"].as_u64().unwrap();
    let incoming = rejection["incoming_frame_bytes"].as_u64().unwrap();
    let limit = rejection["ordinary_byte_limit"].as_u64().unwrap();
    assert!(queued <= limit && queued + incoming > limit);
    assert_eq!(rejection["queued_large_bytes"], 0);
    assert!(rejection["queued_frames"].as_u64().unwrap() > 0);
    assert!(
        !diagnostics.contains("xxxxxxxx"),
        "trace leaked frame contents"
    );
    let _ = tui.get_ref().shutdown(std::net::Shutdown::Both);
    provider.join().unwrap();
    relay.join().unwrap();
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
    assert_eq!(captured.len(), 67);
    assert_eq!(captured[0]["event"]["method"], "turn/start");
    assert_eq!(captured[1]["event"]["method"], "turn/started");
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
    let (relay_server, provider) = UnixStream::pair().unwrap();
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
    assert!(
        request.is_empty(),
        "input crossed the relay without durable evidence"
    );
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
    let (mut hook, receiver) = UnixStream::pair().unwrap();
    hook.write_all(
        format!(
            "{}\n",
            json!({"generation":"generation-1","event":{
        "hook_event_name":"UserPromptSubmit","session_id":"assigned-session",
        "prompt_id":"prompt-2","prompt":"closed-probe"}})
        )
        .as_bytes(),
    )
    .unwrap();
    assert!(receive_hook(receiver, &mut gate, "generation-1", &mut writer).is_err());
    let records: Vec<serde_json::Value> = std::fs::read_to_string(evidence.path())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(records.iter().any(|record| {
        record["execution_generation"] == "generation-1"
            && record["event"]["type"] == "input_rejected"
            && record["event"]["request"]["prompt"] == "closed-probe"
    }));
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
    assert_eq!(lines.len(), 6);
    let first: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
    assert_eq!(first["format"], "fluent-native-agent-event");
    assert_eq!(first["schema_version"], 1);
    assert_eq!(first["provider"], "codex");
    assert_eq!(first["execution_generation"], "generation-1");
    assert_eq!(first["assigned_conversation_id"], "assigned-thread");
    assert_eq!(first["conversation_id"], "assigned-thread");
    assert_eq!(first["event"]["method"], "turn/start");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&lines[1]).unwrap()["event"]["method"],
        "thread/started"
    );
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

    let pi = |event| {
        json!({
            "format":"fluent-native-agent-event","schema_version":1,"provider":"pi",
            "execution_generation":"generation-3","assigned_conversation_id":"assigned-pi",
            "conversation_id":"assigned-pi","model":"local-model",
            "turn_id":null,"input_sequence":1,"event":event,
        })
        .to_string()
    };
    let pi_start = pi(json!({"type":"session_start","reason":"startup"}));
    assert_eq!(
        fluent::transcript::conversation_id_from_line(&pi_start).as_deref(),
        Some("assigned-pi")
    );
    assert!(
        matches!(fluent::transcript::parse_line(&pi_start).as_slice(), [Event::SessionInit { session_id, model }] if session_id == "assigned-pi" && model == "local-model")
    );
    let pi_tool = pi(
        json!({"type":"tool_execution_start","toolCallId":"tool-2","toolName":"bash","args":{"command":"git status"}}),
    );
    assert!(
        matches!(fluent::transcript::parse_line(&pi_tool).as_slice(), [Event::ToolUse { id, name, .. }] if id == "tool-2" && name == "bash")
    );
    let pi_message = pi(
        json!({"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"Pi answer"}]}}),
    );
    assert!(
        matches!(fluent::transcript::parse_line(&pi_message).as_slice(), [Event::Text { text }] if text == "Pi answer")
    );
    let pi_usage = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(pi_usage.path(), format!("{}\n", pi(json!({"type":"turn_end","message":{"model":"local-model","timestamp":1781715768038_u64,"usage":{"input":42,"output":7,"cacheRead":3}}})))).unwrap();
    let rows = fluent::usage::extract_pi_usage(pi_usage.path(), "work-1", "attempt-1", "write-1");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].input_tokens, 42);
    assert_eq!(rows[0].output_tokens, 7);

    let claude_session = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(claude_session.path(), concat!(
        "{\"type\":\"assistant\",\"message\":{\"id\":\"msg-1\",\"model\":\"claude-assigned\",\"usage\":{\"input_tokens\":12,\"output_tokens\":2,\"cache_read_input_tokens\":1}}}\n",
        "{\"type\":\"assistant\",\"message\":{\"id\":\"msg-1\",\"model\":\"claude-assigned\",\"usage\":{\"input_tokens\":12,\"output_tokens\":8,\"cache_read_input_tokens\":1}}}\n"
    )).unwrap();
    let claude_rows = fluent::usage::extract_native_claude_session_usage(
        claude_session.path(),
        "work-1",
        "attempt-1",
        "write-1",
    );
    assert_eq!(claude_rows.len(), 1);
    assert_eq!(claude_rows[0].output_tokens, 8);

    // Feed the captured Codex events through the remaining production readers.
    let project = tempfile::tempdir().unwrap();
    let workspace = project.path().join("candidate");
    std::fs::create_dir_all(workspace.join("src")).unwrap();
    std::fs::write(
        workspace.join("src/lib.rs"),
        "pub fn answer() -> u8 { 42 }\n",
    )
    .unwrap();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(&workspace)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["add", "src/lib.rs"]);
    git(&[
        "-c",
        "user.name=Native Fixture",
        "-c",
        "user.email=native@example.invalid",
        "commit",
        "-q",
        "-m",
        "Add fixture",
    ]);
    let commit = git(&["rev-parse", "HEAD"]);
    let transcript = project
        .path()
        .join(".fluent/work/artifacts/work-1/attempt-1/attempt-1-write-1/transcript.jsonl");
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    let command_event = json!({
        "format": "fluent-native-agent-event",
        "schema_version": 1,
        "provider": "codex",
        "execution_generation": "generation-1",
        "assigned_conversation_id": "assigned-thread",
        "conversation_id": "assigned-thread",
        "turn_id": "turn-1",
        "input_sequence": 1,
        "event": {"method": "item/completed", "params": {
            "threadId": "assigned-thread", "turnId": "turn-1",
            "item": {"id": "command-1", "type": "commandExecution",
                "command": format!("git show '{commit}:src/lib.rs'"),
                "exitCode": 0, "aggregatedOutput": "pub fn answer() -> u8 { 42 }\n"}
        }}
    });
    std::fs::write(
        &transcript,
        format!("{}\n{command_event}\n", lines.join("\n")),
    )
    .unwrap();
    let inspection = workspace.join("review-inspection.json");
    std::fs::write(
        &inspection,
        json!({
            "schema-version": 1,
            "candidate-commit": commit,
            "role": "tests",
            "inspections": [{"path": "src/lib.rs", "summary": "Checked the candidate source file."}]
        })
        .to_string(),
    )
    .unwrap();
    fluent::work_task_executor::native_review_inspection_for_test(
        &inspection,
        &workspace,
        &transcript,
        &commit,
    )
    .unwrap();
    let prompt =
        fluent::work_task_executor::native_review_prompt_for_test(project.path(), &transcript)
            .unwrap();
    assert!(prompt.contains("Writer transcript"));
    assert!(prompt.contains(&transcript.display().to_string()));

    let recovery = project.path().join("sessions/session-1/transcript.jsonl");
    std::fs::create_dir_all(recovery.parent().unwrap()).unwrap();
    std::fs::copy(&transcript, &recovery).unwrap();
    let latest = fluent::transcript::find_latest_transcript(project.path()).unwrap();
    assert_eq!(latest, recovery);
    let mut reader = fluent::transcript::TranscriptReader::new(latest);
    assert!(
        reader
            .read_new()
            .iter()
            .any(|event| matches!(event, Event::Text { text } if text == "Task answer"))
    );
    assert!(reader.read_new().is_empty());
}

#[test]
fn continuation_reuses_conversation_not_live_terminal_identity() {
    if std::env::var("FLUENT_TEST_REAL_NATIVE_AGENT_HOST").as_deref() == Ok("1") {
        host_native_task_continuation_reuses_saved_conversation();
        return;
    }
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

struct HostTaskChildGuard {
    child: std::process::Child,
    binary: std::path::PathBuf,
    project: std::path::PathBuf,
    work: String,
    runtime: std::path::PathBuf,
    reap_timeout: std::time::Duration,
    armed: bool,
}

fn host_first_turn_record(transcript: &str, generation: &str) -> Option<serde_json::Value> {
    let mut records = transcript
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok());
    let belongs_to_first_turn = |record: &serde_json::Value| {
        record["format"] == "fluent-native-agent-event"
            && record["schema_version"] == 1
            && record["provider"] == "codex"
            && record["execution_generation"] == generation
            && record["input_sequence"] == 1
            && record["turn_id"].as_str().is_some_and(|id| !id.is_empty())
            && record["assigned_conversation_id"]
                .as_str()
                .is_some_and(|id| !id.is_empty())
            && record["conversation_id"] == record["assigned_conversation_id"]
            && record["event"]["params"]["threadId"] == record["assigned_conversation_id"]
            && record["event"]["params"]["turn"]["id"] == record["turn_id"]
    };
    let started = records.find(|record| {
        belongs_to_first_turn(record) && record["event"]["method"] == "turn/started"
    })?;
    records.find(|record| {
        belongs_to_first_turn(record)
            && record["assigned_conversation_id"] == started["assigned_conversation_id"]
            && record["turn_id"] == started["turn_id"]
            && record["event"]["method"] == "turn/completed"
            && record["event"]["params"]["turn"]["status"] == "completed"
    })
}

fn host_first_turn_fixture(method: &str) -> serde_json::Value {
    json!({
        "format": "fluent-native-agent-event", "schema_version": 1, "provider": "codex",
        "execution_generation": "current-generation", "assigned_conversation_id": "thread-1",
        "conversation_id": "thread-1", "input_sequence": 1, "turn_id": "first-turn",
        "event": {"method": method, "params": {"threadId": "thread-1",
            "turn": {"id": "first-turn", "status": if method == "turn/started" { "inProgress" } else { "completed" }}}}
    })
}

#[test]
fn host_first_turn_probe_requires_successful_completion() {
    let started = host_first_turn_fixture("turn/started");
    let completed = host_first_turn_fixture("turn/completed");
    assert!(host_first_turn_record(&started.to_string(), "current-generation").is_none());
    assert!(host_first_turn_record(&completed.to_string(), "current-generation").is_none());
    assert!(
        host_first_turn_record(&format!("{completed}\n{started}"), "current-generation").is_none()
    );
    let transcript = format!("{started}\n{completed}");
    let record = host_first_turn_record(&transcript, "current-generation").unwrap();
    assert_eq!(record["event"]["method"], "turn/completed");
    assert_eq!(record["event"]["params"]["turn"]["status"], "completed");
}

#[test]
fn host_first_turn_probe_rejects_failed_or_uncorrelated_completion() {
    let started = host_first_turn_fixture("turn/started");
    for (pointer, value) in [
        ("/event/params/turn/status", json!("failed")),
        ("/event/params/turn/status", json!("interrupted")),
        ("/event/params/turn/status", serde_json::Value::Null),
        ("/execution_generation", json!("old-generation")),
        ("/assigned_conversation_id", json!("other-thread")),
        ("/conversation_id", json!("other-thread")),
        ("/event/params/threadId", json!("other-thread")),
        ("/turn_id", json!("other-turn")),
        ("/event/params/turn/id", json!("other-turn")),
        ("/input_sequence", json!(2)),
        ("/format", json!("other-format")),
        ("/provider", json!("other-provider")),
        ("/schema_version", json!(2)),
    ] {
        let mut completed = host_first_turn_fixture("turn/completed");
        *completed.pointer_mut(pointer).unwrap() = value;
        let transcript = format!("{started}\n{completed}");
        assert!(
            host_first_turn_record(&transcript, "current-generation").is_none(),
            "accepted mismatch at {pointer}: {completed}"
        );
    }
    // Internally consistent records from another conversation or turn also fail.
    for pointers in [
        vec![
            "/assigned_conversation_id",
            "/conversation_id",
            "/event/params/threadId",
        ],
        vec!["/turn_id", "/event/params/turn/id"],
    ] {
        let mut completed = host_first_turn_fixture("turn/completed");
        for pointer in pointers {
            *completed.pointer_mut(pointer).unwrap() = json!("unrelated");
        }
        assert!(
            host_first_turn_record(&format!("{started}\n{completed}"), "current-generation")
                .is_none()
        );
    }
}

#[test]
fn host_first_turn_probe_ignores_prior_generation() {
    let mut old_start = host_first_turn_fixture("turn/started");
    let mut old_end = host_first_turn_fixture("turn/completed");
    old_start["execution_generation"] = json!("old-generation");
    old_end["execution_generation"] = json!("old-generation");
    let started = host_first_turn_fixture("turn/started");
    let completed = host_first_turn_fixture("turn/completed");
    let transcript = format!("{old_start}\n{old_end}\n{started}\n{completed}");
    assert!(host_first_turn_record(&transcript, "other-generation").is_none());
    let record = host_first_turn_record(&transcript, "current-generation").unwrap();
    assert_eq!(record["execution_generation"], "current-generation");
    assert_eq!(record["event"]["method"], "turn/completed");
}

impl Drop for HostTaskChildGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        // Cancel through the public owner before reaping the fixture's child.
        // The fixture owns this disposable Attempt and retains the cancel log.
        let cancel_log = self.runtime.join("task.cleanup.log");
        let cancel_error = self.runtime.join("task.cleanup.stderr.log");
        let mut command = std::process::Command::new(&self.binary);
        command
            .args(["attempt", "cancel", &self.work, "attempt-1"])
            .current_dir(&self.project);
        if let Ok(stdout) = std::fs::File::create(cancel_log) {
            command.stdout(std::process::Stdio::from(stdout));
        }
        if let Ok(stderr) = std::fs::File::create(cancel_error) {
            command.stderr(std::process::Stdio::from(stderr));
        }
        if let Ok(mut cancel) = command.spawn() {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
            while cancel.try_wait().ok().flatten().is_none() && std::time::Instant::now() < deadline
            {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            let _ = cancel.kill();
            let _ = cancel.wait();
        }
        let deadline = std::time::Instant::now() + self.reap_timeout;
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                _ => break,
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn host_fixture_child_guard_reaps_on_panic() {
    let root = tempfile::tempdir().unwrap();
    let child = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let pid = child.id() as i32;
    let result = std::panic::catch_unwind(|| {
        let _guard = HostTaskChildGuard {
            child,
            binary: "/usr/bin/false".into(),
            project: root.path().into(),
            work: "fixture-only".into(),
            runtime: root.path().into(),
            reap_timeout: std::time::Duration::from_millis(50),
            armed: true,
        };
        panic!("fixture assertion failed");
    });
    assert!(result.is_err());
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
}

const HOST_NATIVE_ATTACH: &str = r#"
set timeout 20
if {$env(FLUENT_NATIVE_CONTROL) eq "1"} {
    spawn -noecho $env(FLUENT_NATIVE_BINARY) terminal attach --source $env(FLUENT_NATIVE_SOURCE) $env(FLUENT_NATIVE_SESSION) --control
} else {
    spawn -noecho $env(FLUENT_NATIVE_BINARY) terminal attach --source $env(FLUENT_NATIVE_SOURCE) $env(FLUENT_NATIVE_SESSION)
}

if {[info exists env(FLUENT_NATIVE_FIXTURE_READY)]} {
    expect "fixture-ready"
}
if {$env(FLUENT_NATIVE_CONTROL) eq "1" && $env(FLUENT_NATIVE_PROVIDER) eq "codex"} {
    # The attached tmux client can be running before the Codex composer exists.
    # Wait for its rendered prompt before queuing input for the assigned turn.
    expect {
        "›" {}
        eof { exit 1 }
        timeout { exit 1 }
    }
}
after 300
if {$env(FLUENT_NATIVE_CONTROL) eq "1"} {
    send -- "Please retain result.txt and finish the Task after committing."
    # Codex treats a rapid text burst followed immediately by Enter as a
    # multiline paste. Wait for its composer to flush the burst before submit.
    if {$env(FLUENT_NATIVE_PROVIDER) eq "codex"} { after 300 }
    send -- "\r"
    after 300
    send -- "The final file must have exactly the requested line."
    if {$env(FLUENT_NATIVE_PROVIDER) eq "codex"} { after 300 }
    if {$env(FLUENT_NATIVE_PROVIDER) eq "pi"} {
        send -- "\033\r"
    } else {
        send -- "\r"
    }
    after 300
}
send -- "\002d"
expect eof
set outcome [wait]
exit [lindex $outcome 3]
"#;

#[test]
fn host_fixture_requests_completion_after_attached_input() {
    for provider in ["codex", "claude", "pi"] {
        let instructions =
            host_native_instructions(provider, std::path::Path::new("/tmp/forbidden"));
        assert!(instructions.contains("Give a final response after committing"));
    }
    assert!(HOST_NATIVE_ATTACH.contains("finish the Task after committing"));
    assert!(!HOST_NATIVE_ATTACH.contains("result.txt and continue"));
}

const HOST_NATIVE_RACE_CONTROL: &str = r#"
proc trace_race {message} {
    global env
    if {![info exists env(FLUENT_NATIVE_RACE_STATE)]} { return }
    set trace [open $env(FLUENT_NATIVE_RACE_STATE) a]
    puts $trace $message
    close $trace
}
set timeout 60
spawn -noecho $env(FLUENT_NATIVE_BINARY) terminal attach --source $env(FLUENT_NATIVE_SOURCE) $env(FLUENT_NATIVE_SESSION) --control
trace_race "attachment-pid=[exp_pid]"
if {$env(FLUENT_NATIVE_PROVIDER) eq "codex"} {
    set timeout 15
    expect {
        "›" {}
        eof { exit 1 }
        timeout { exit 1 }
    }
    send -- $env(FLUENT_NATIVE_RACE_TEXT)
    # Let the installed composer finish its paste burst before Enter races
    # with completion. Otherwise the burst can become a multiline draft.
    after 300
} else {
    set timeout 1
    expect { eof { exit 1 } timeout {} }
}
set ready [open $env(FLUENT_NATIVE_RACE_READY) w]
puts $ready "attached"
close $ready
while {![file exists $env(FLUENT_NATIVE_RACE_RELEASE)]} { after 25 }
if {$env(FLUENT_NATIVE_PROVIDER) eq "codex"} {
    set result [catch {send -- "\r"} error]
} else {
    set result [catch {send -- "$env(FLUENT_NATIVE_RACE_TEXT)\r"} error]
}
set attempt [open $env(FLUENT_NATIVE_RACE_ATTEMPT) w]
puts $attempt "$result $error"
close $attempt
# Keep this attachment on the Writer terminal through settlement. Detaching
# here would make EOF mean only that the operator left the terminal.
set timeout 60
trace_race "waiting-for-attachment-eof"
expect {
    eof { trace_race "attachment-eof" }
    timeout { trace_race "attachment-eof-timeout"; exit 124 }
}
trace_race "waiting-for-attachment-child"
set outcome [wait]
trace_race "attachment-reaped"
exit [lindex $outcome 3]
"#;

fn host_native_race_command(provider: &str) -> std::process::Command {
    let mut command = std::process::Command::new("expect");
    command
        .args(["-c", HOST_NATIVE_RACE_CONTROL])
        .env("FLUENT_NATIVE_PROVIDER", provider);
    command
}

fn host_native_instructions(provider: &str, forbidden: &std::path::Path) -> String {
    format!(
        "First try to run `touch {}` with the shell tool and record that the source checkout denies this write. Then run sleep 8 with the shell tool so an operator can attach and send steering and follow-up input. Create result.txt containing exactly native-{provider} followed by a newline. Commit the file. Give a final response after committing. Keep the change small.",
        forbidden.display()
    )
}

#[test]
fn host_native_race_passes_provider_to_expect() {
    for provider in ["codex", "claude", "pi"] {
        let command = host_native_race_command(provider);
        let configured = command
            .get_envs()
            .find(|(key, _)| *key == "FLUENT_NATIVE_PROVIDER")
            .and_then(|(_, value)| value);
        assert_eq!(configured, Some(std::ffi::OsStr::new(provider)));
    }
}

#[test]
fn host_claude_race_observes_eof_before_reaping_attachment() {
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, Instant};

    let root = tempfile::tempdir().unwrap();
    let terminal = root.path().join("fake-terminal");
    let ready = root.path().join("race.ready");
    let release = root.path().join("race.release");
    let attempt = root.path().join("race.attempt");
    let state = root.path().join("race.state");
    std::fs::write(&terminal, "#!/bin/sh\nsleep 4\n").unwrap();
    std::fs::set_permissions(&terminal, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut child = host_native_race_command("claude")
        .env("FLUENT_NATIVE_BINARY", &terminal)
        .env("FLUENT_NATIVE_SOURCE", root.path())
        .env("FLUENT_NATIVE_SESSION", "fixture")
        .env("FLUENT_NATIVE_RACE_READY", &ready)
        .env("FLUENT_NATIVE_RACE_RELEASE", &release)
        .env("FLUENT_NATIVE_RACE_ATTEMPT", &attempt)
        .env("FLUENT_NATIVE_RACE_STATE", &state)
        .env("FLUENT_NATIVE_RACE_TEXT", "race-marker")
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    while !ready.exists() {
        assert!(Instant::now() < deadline, "Claude fixture did not attach");
        std::thread::sleep(Duration::from_millis(10));
    }
    std::fs::write(&release, b"send").unwrap();
    let status = child.wait().unwrap();
    assert!(status.success());
    assert!(attempt.is_file());
    let trace = std::fs::read_to_string(state).unwrap();
    assert!(trace.contains("attachment-eof\n"), "{trace}");
    assert!(trace.contains("attachment-reaped\n"), "{trace}");
}

#[test]
fn host_codex_race_submits_prepared_input_at_boundary() {
    assert_host_codex_race_submits_prepared_input_at_boundary(0);
}

#[test]
fn host_codex_race_submits_prepared_input_after_slow_startup() {
    assert_host_codex_race_submits_prepared_input_at_boundary(4);
}

fn assert_host_codex_race_submits_prepared_input_at_boundary(startup_delay_secs: u64) {
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, Instant};

    let root = tempfile::tempdir().unwrap();
    let terminal = root.path().join("fake-terminal");
    let ready = root.path().join("race.ready");
    let release = root.path().join("race.release");
    let attempt = root.path().join("race.attempt");
    let capture = root.path().join("capture.txt");
    let expect_log = root.path().join("expect.log");
    std::fs::write(
        &terminal,
        r#"#!/usr/bin/env python3
import os, select, sys, termios, time, tty
fd = sys.stdin.fileno()
old = termios.tcgetattr(fd)
try:
    tty.setraw(fd)
    time.sleep(float(os.environ['FLUENT_NATIVE_STARTUP_DELAY']))
    print('›', flush=True)
    marker = os.environ['FLUENT_NATIVE_RACE_TEXT'].encode()
    data = bytearray()
    while marker not in data:
        data.extend(os.read(fd, 1))
    with open(os.environ['FLUENT_NATIVE_CAPTURE'], 'w') as out:
        out.write('before' if not os.path.exists(os.environ['FLUENT_NATIVE_RACE_RELEASE']) else 'after')
    while not data.endswith(b'\r'):
        data.extend(os.read(fd, 1))
    if select.select([fd], [], [], 0.5)[0]:
        data.extend(os.read(fd, 16))
    with open(os.environ['FLUENT_NATIVE_CAPTURE'], 'a') as out:
        out.write('\n' + repr(bytes(data)))
finally:
    termios.tcsetattr(fd, termios.TCSADRAIN, old)
"#,
    )
    .unwrap();
    std::fs::set_permissions(&terminal, std::fs::Permissions::from_mode(0o755)).unwrap();
    let expect_output = std::fs::File::create(&expect_log).unwrap();
    let mut child = host_native_race_command("codex")
        .env("FLUENT_NATIVE_BINARY", &terminal)
        .env("FLUENT_NATIVE_SOURCE", root.path())
        .env("FLUENT_NATIVE_SESSION", "fixture")
        .env("FLUENT_NATIVE_RACE_READY", &ready)
        .env("FLUENT_NATIVE_RACE_RELEASE", &release)
        .env("FLUENT_NATIVE_RACE_ATTEMPT", &attempt)
        .env("FLUENT_NATIVE_RACE_TEXT", "race-marker")
        .env("FLUENT_NATIVE_CAPTURE", &capture)
        .env(
            "FLUENT_NATIVE_STARTUP_DELAY",
            startup_delay_secs.to_string(),
        )
        .stdout(expect_output.try_clone().unwrap())
        .stderr(expect_output)
        .spawn()
        .unwrap();
    // Startup does not measure the input race. Wait for both Expect's ready
    // marker and the complete draft capture before releasing Enter.
    let deadline = Instant::now() + Duration::from_secs(60);
    while !ready.is_file() || !std::fs::read_to_string(&capture).is_ok_and(|text| text == "before")
    {
        let exit = child.try_wait().unwrap();
        if exit.is_some() || Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "Codex fixture did not prepare input: exit={exit:?}; capture={:?}; expect={}",
                std::fs::read_to_string(&capture),
                std::fs::read_to_string(&expect_log).unwrap()
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    std::fs::write(&release, b"send").unwrap();
    let status = child.wait().unwrap();
    assert!(status.success());
    let capture = std::fs::read_to_string(capture).unwrap();
    assert!(
        capture.starts_with("before\n"),
        "{capture}; expect: {}",
        std::fs::read_to_string(expect_log).unwrap()
    );
    assert!(capture.contains("b'race-marker\\r'"), "{capture}");
    assert!(std::fs::read_to_string(attempt).unwrap().starts_with("0 "));
}

#[test]
fn host_codex_attachment_queues_both_inputs() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().unwrap();
    let capture = root.path().join("input.bin");
    let submissions = root.path().join("submissions.txt");
    let early = root.path().join("early-input");
    let terminal = root.path().join("fake-terminal");
    std::fs::write(
        &terminal,
        "#!/usr/bin/env python3\nimport os, select, sys, termios, time, tty\nfd = sys.stdin.fileno()\nold = termios.tcgetattr(fd)\ntry:\n    tty.setraw(fd)\n    print('fixture-ready', flush=True)\n    time.sleep(1)\n    if select.select([fd], [], [], 0)[0]:\n        open(os.environ['FLUENT_NATIVE_EARLY'], 'w').close()\n    print('›', flush=True)\n    data = bytearray()\n    draft = bytearray()\n    submitted = []\n    last_text = None\n    while not data.endswith(b'\\x02d'):\n        byte = os.read(fd, 1)\n        data.extend(byte)\n        now = time.monotonic()\n        if byte == b'\\r':\n            if last_text is not None and now - last_text >= 0.2:\n                submitted.append(draft.decode())\n                draft.clear()\n            else:\n                draft.extend(b'\\n')\n        elif byte not in (b'\\x02',) and not data.endswith(b'\\x02d'):\n            draft.extend(byte)\n            last_text = now\n    with open(os.environ['FLUENT_NATIVE_CAPTURE'], 'wb') as out:\n        out.write(data)\n    with open(os.environ['FLUENT_NATIVE_SUBMISSIONS'], 'w') as out:\n        out.write('\\n'.join(submitted))\nfinally:\n    termios.tcsetattr(fd, termios.TCSADRAIN, old)\n",
    )
    .unwrap();
    std::fs::set_permissions(&terminal, std::fs::Permissions::from_mode(0o755)).unwrap();
    let output = std::process::Command::new("expect")
        .args(["-c", HOST_NATIVE_ATTACH])
        .env("FLUENT_NATIVE_BINARY", &terminal)
        .env("FLUENT_NATIVE_SOURCE", root.path())
        .env("FLUENT_NATIVE_SESSION", "fixture")
        .env("FLUENT_NATIVE_CONTROL", "1")
        .env("FLUENT_NATIVE_PROVIDER", "codex")
        .env("FLUENT_NATIVE_FIXTURE_READY", "1")
        .env("FLUENT_NATIVE_CAPTURE", &capture)
        .env("FLUENT_NATIVE_SUBMISSIONS", &submissions)
        .env("FLUENT_NATIVE_EARLY", &early)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(
        std::fs::read(capture).unwrap(),
        b"Please retain result.txt and finish the Task after committing.\rThe final file must have exactly the requested line.\r\x02d"
    );
    assert!(
        !early.exists(),
        "attached input preceded the Codex composer"
    );
    assert_eq!(
        std::fs::read_to_string(submissions).unwrap(),
        "Please retain result.txt and finish the Task after committing.\nThe final file must have exactly the requested line."
    );
}

fn pi_pane_generation(pane: &str) -> (std::path::PathBuf, String) {
    let generation_file = pane
        .lines()
        .find(|line| line.starts_with("export FLUENT_NATIVE_PI_GENERATION="))
        .and_then(|line| line.split('\'').nth(1))
        .map(std::path::PathBuf::from)
        .expect("Pi pane omits owner generation file");
    let generation =
        std::fs::read_to_string(&generation_file).expect("Pi owner generation file is unreadable");
    (generation_file, generation)
}

#[test]
fn pi_host_pane_reads_generation_id_from_file() {
    let runtime = tempfile::tempdir().unwrap();
    let generation_file = runtime.path().join("generation");
    std::fs::write(&generation_file, "assigned-generation").unwrap();
    let pane = format!(
        "export FLUENT_NATIVE_PI_GENERATION=$(cat '{}')\n",
        generation_file.display()
    );

    let (path, generation) = pi_pane_generation(&pane);
    assert_eq!(path, generation_file);
    assert_eq!(generation, "assigned-generation");
}

struct HostTaskClock {
    deadline: std::time::Instant,
}

impl HostTaskClock {
    fn new(started: std::time::Instant) -> Self {
        Self {
            deadline: started + std::time::Duration::from_secs(600),
        }
    }

    fn begin_settlement(&mut self, control_attached: std::time::Instant) {
        self.deadline = control_attached + std::time::Duration::from_secs(600);
    }

    fn expired(&self, now: std::time::Instant) -> bool {
        now >= self.deadline
    }
}

#[test]
fn host_task_completion_gets_full_window_after_control_attaches() {
    use std::time::{Duration, Instant};

    let started = Instant::now();
    let mut clock = HostTaskClock::new(started);
    let control_attached = started + Duration::from_secs(300);
    clock.begin_settlement(control_attached);

    assert!(!clock.expired(started + Duration::from_secs(601)));
    assert!(!clock.expired(control_attached + Duration::from_secs(599)));
    assert!(clock.expired(control_attached + Duration::from_secs(600)));
}

/// Run only from the separately authorized host check. The fixture stays in
/// its output directory so the provider transcript and candidate can be
/// inspected together after the test exits.
#[test]
#[ignore]
fn host_native_agent_acceptance() {
    use std::fs::{self, File};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    struct ProxyGuard {
        child: std::process::Child,
        output: std::path::PathBuf,
    }
    impl Drop for ProxyGuard {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    assert_eq!(
        std::env::var("FLUENT_TEST_REAL_NATIVE_AGENT_HOST").unwrap(),
        "1"
    );
    let first_turn_only =
        std::env::var("FLUENT_TEST_NATIVE_CODEX_FIRST_TURN_ONLY").is_ok_and(|value| value == "1");
    let output_root =
        std::path::PathBuf::from(std::env::var("FLUENT_NATIVE_HOST_OUTPUT_DIR").unwrap());
    fs::create_dir_all(&output_root).unwrap();
    let binary = env!("CARGO_BIN_EXE_fluent");
    for (provider, model_var, effort_var) in [
        (
            "codex",
            "FLUENT_NATIVE_CODEX_MODEL",
            "FLUENT_NATIVE_CODEX_EFFORT",
        ),
        (
            "claude",
            "FLUENT_NATIVE_CLAUDE_MODEL",
            "FLUENT_NATIVE_CLAUDE_EFFORT",
        ),
        ("pi", "FLUENT_NATIVE_PI_MODEL", "FLUENT_NATIVE_PI_EFFORT"),
    ] {
        let model = std::env::var(model_var).unwrap();
        let effort = std::env::var(effort_var).unwrap();
        let project = output_root.join(provider);
        // Keep logs and coordination files outside the source checkout so
        // every native Task starts with a clean Git worktree.
        let runtime = output_root.join(format!("{provider}-runtime"));
        fs::create_dir(&project).unwrap();
        fs::create_dir(&runtime).unwrap();
        let run = |program: &str, args: &[&str]| {
            let output = Command::new(program)
                .args(args)
                .current_dir(&project)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{provider} fixture command {program} {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        run("git", &["init", "-b", "main"]);
        run("git", &["config", "user.name", "Native Fixture"]);
        run("git", &["config", "user.email", NATIVE_FIXTURE_GIT_EMAIL]);
        run("git", &["config", "commit.gpgsign", "false"]);
        fs::write(
            project.join("README.md"),
            "Native Task acceptance fixture.\n",
        )
        .unwrap();
        run("git", &["add", "README.md"]);
        run("git", &["commit", "-m", "Add fixture"]);
        let work = format!("native-{provider}");
        let forbidden = project.join("forbidden.txt");
        let instructions = host_native_instructions(provider, &forbidden);
        run(
            binary,
            &[
                "work-item",
                "create",
                &work,
                "--title",
                "Native Task acceptance",
                "--instructions",
                &instructions,
            ],
        );
        run(
            binary,
            &[
                "attempt",
                "create",
                &work,
                "attempt-1",
                "--terminal-mode",
                "native",
                "--write-coder",
                provider,
                "--write-model",
                &model,
                "--write-effort",
                &effort,
                "--review-coder",
                provider,
                "--review-model",
                &model,
                "--review-effort",
                &effort,
            ],
        );
        let mut pi_proxy = None;
        let mut pi_endpoint = std::env::var("FLUENT_NATIVE_PI_ENDPOINT").unwrap_or_default();
        if provider == "pi" {
            let proxy_output = runtime.join("retry-proxy");
            fs::create_dir(&proxy_output).unwrap();
            let proxy_log = File::create(proxy_output.join("proxy.log")).unwrap();
            let proxy = Command::new("python3")
                .arg(
                    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("tests/host/pi-retry-proxy.py"),
                )
                .env("FLUENT_NATIVE_PI_PROXY_OUTPUT", &proxy_output)
                .env(
                    "FLUENT_NATIVE_PI_REJECTED_TEXT",
                    "pi-input-after-receiver-loss",
                )
                .stdout(Stdio::from(proxy_log))
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            let port = loop {
                if let Ok(port) = fs::read_to_string(proxy_output.join("port.txt")) {
                    break port;
                }
                assert!(Instant::now() < deadline, "Pi retry proxy did not start");
                std::thread::sleep(Duration::from_millis(50));
            };
            pi_endpoint = format!("http://127.0.0.1:{}/v1", port.trim());
            pi_proxy = Some(ProxyGuard {
                child: proxy,
                output: proxy_output,
            });
        }
        let log = File::create(runtime.join("task.stdout.log")).unwrap();
        let error_log = File::create(runtime.join("task.stderr.log")).unwrap();
        let child = Command::new(binary)
            .args(["task", "run", &work, "attempt-1", "attempt-1-write-1"])
            .current_dir(&project)
            .env("FLUENT_NATIVE_PI_ENDPOINT", &pi_endpoint)
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(error_log))
            .spawn()
            .unwrap();
        let mut child = HostTaskChildGuard {
            child,
            binary: binary.into(),
            project: project.clone(),
            work: work.clone(),
            runtime: runtime.clone(),
            reap_timeout: Duration::from_secs(10),
            armed: true,
        };
        let mut clock = HostTaskClock::new(Instant::now());
        let artifact = project.join(format!(
            ".fluent/work/artifacts/{work}/attempt-1/attempt-1-write-1"
        ));
        let session = loop {
            assert!(
                child.child.try_wait().unwrap().is_none(),
                "{provider} native Task ended before attachment; inspect {} and {}",
                project.display(),
                runtime.display()
            );
            let output = Command::new(binary)
                .args(["terminal", "list", "--source", project.to_str().unwrap()])
                .output()
                .unwrap();
            let transcript =
                fs::read_to_string(artifact.join("transcript.jsonl")).unwrap_or_default();
            let records: Vec<serde_json::Value> = transcript
                .lines()
                .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                .collect();
            let mut last_sequence = 0;
            let last_protocol: Vec<_> = records
                .iter()
                .rev()
                .take(5)
                .map(|record| {
                    record["event"]["method"]
                        .as_str()
                        .or_else(|| record["event"]["type"].as_str())
                        .unwrap_or("response")
                        .to_string()
                })
                .collect();
            let mut last_terminal = None;
            if output.status.success()
                && let Ok(list) = serde_json::from_slice::<serde_json::Value>(&output.stdout)
                && let Some(descriptor) = list.as_array().and_then(|items| {
                    items.iter().find(|item| {
                        item["task_id"] == "attempt-1-write-1" && item["can_observe"] == true
                    })
                })
                && let Some(session) = descriptor["session_id"].as_str()
                && let Some(generation) = descriptor["execution_generation"].as_str()
            {
                last_terminal = Some(descriptor.clone());
                last_sequence = records
                    .iter()
                    .filter(|record| record["execution_generation"] == generation)
                    .filter_map(|record| record["input_sequence"].as_u64())
                    .max()
                    .unwrap_or(0);
                if last_sequence >= 1 {
                    break (session.to_string(), generation.to_string());
                }
            }
            assert!(
                !clock.expired(Instant::now()),
                "{provider} native Task timed out waiting for {}: terminal={last_terminal:?}, max_input_sequence={last_sequence}, recent_protocol={last_protocol:?}, task_pid={}, task_alive=true, inspect {} and {}",
                if last_terminal.is_none() {
                    "terminal descriptor"
                } else {
                    "launch prompt admission"
                },
                child.child.id(),
                project.display(),
                runtime.display()
            );
            std::thread::sleep(Duration::from_millis(100));
        };
        let (session, generation) = session;
        if first_turn_only {
            assert_eq!(provider, "codex");
            let first_turn_deadline = Instant::now() + Duration::from_secs(300);
            loop {
                let transcript =
                    fs::read_to_string(artifact.join("transcript.jsonl")).unwrap_or_default();
                if let Some(record) = host_first_turn_record(&transcript, &generation) {
                    fs::write(
                        runtime.join("first-turn.json"),
                        serde_json::to_vec_pretty(&json!({
                            "execution_generation": record["execution_generation"],
                            "assigned_conversation_id": record["assigned_conversation_id"],
                            "turn_id": record["turn_id"],
                            "input_sequence": record["input_sequence"],
                            "completion_status": record["event"]["params"]["turn"]["status"],
                            "completion_event": record["event"],
                            "terminal_session": session,
                        }))
                        .unwrap(),
                    )
                    .unwrap();
                    return;
                }
                assert!(
                    child.child.try_wait().unwrap().is_none(),
                    "Codex exited before successful first-turn completion; inspect {} and {}",
                    artifact.display(),
                    runtime.display()
                );
                assert!(
                    Instant::now() < first_turn_deadline,
                    "Codex did not complete its first turn successfully; inspect {} and {}",
                    artifact.display(),
                    runtime.display()
                );
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        if provider == "pi" {
            let source_dir = std::env::var_os("PI_CODING_AGENT_DIR")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| {
                    std::path::PathBuf::from(std::env::var("HOME").unwrap()).join(".pi/agent")
                });
            let source: serde_json::Value =
                serde_json::from_slice(&fs::read(source_dir.join("models.json")).unwrap()).unwrap();
            let home = fs::read_dir(&artifact)
                .unwrap()
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .find(|path| {
                    path.file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with("native-pi-home-")
                })
                .expect("Pi private agent home is absent");
            let private: serde_json::Value =
                serde_json::from_slice(&fs::read(home.join("models.json")).unwrap()).unwrap();
            let selected = source["providers"]["local-openai"]["models"]
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["id"] == model)
                .unwrap();
            assert_eq!(
                private["providers"]["local-openai"]["models"],
                json!([selected])
            );
            assert_eq!(private["providers"]["local-openai"]["baseUrl"], pi_endpoint);
            assert_eq!(private["providers"].as_object().unwrap().len(), 1);
        }
        for (label, control) in [
            ("observer-before", false),
            ("control", true),
            ("observer-after", false),
        ] {
            let log = File::create(runtime.join(format!("{label}.log"))).unwrap();
            let status = Command::new("expect")
                .args(["-c", HOST_NATIVE_ATTACH])
                .env("FLUENT_NATIVE_BINARY", binary)
                .env("FLUENT_NATIVE_SOURCE", &project)
                .env("FLUENT_NATIVE_SESSION", &session)
                .env("FLUENT_NATIVE_CONTROL", if control { "1" } else { "0" })
                .env("FLUENT_NATIVE_PROVIDER", provider)
                .stdout(Stdio::from(log))
                .status()
                .unwrap();
            assert!(
                status.success(),
                "{provider} {label} attachment failed; inspect {} and {}",
                project.display(),
                runtime.display()
            );
        }
        // Start a second control attachment before the Writer settles, then
        // release one input when its provider reports the settling boundary.
        // The input may be admitted to this generation or rejected; it must
        // never become input for the following Reviewer.
        let boundary = match provider {
            "codex" => "turn/completed",
            "claude" => "\"Stop\"",
            "pi" => "agent_before_settle",
            _ => unreachable!(),
        };
        let previous_boundaries = fs::read_to_string(artifact.join("transcript.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter(|line| line.contains(boundary))
            .count();
        let race_marker = format!("native-{provider}-settlement-probe");
        let race_ready = runtime.join("race.ready");
        let race_release = runtime.join("race.release");
        let race_attempt = runtime.join("race.attempt");
        let mut race = host_native_race_command(provider)
            .env("FLUENT_NATIVE_BINARY", binary)
            .env("FLUENT_NATIVE_SOURCE", &project)
            .env("FLUENT_NATIVE_SESSION", &session)
            .env("FLUENT_NATIVE_RACE_READY", &race_ready)
            .env("FLUENT_NATIVE_RACE_RELEASE", &race_release)
            .env("FLUENT_NATIVE_RACE_ATTEMPT", &race_attempt)
            .env("FLUENT_NATIVE_RACE_STATE", runtime.join("race.state"))
            .env("FLUENT_NATIVE_RACE_TEXT", &race_marker)
            .stdout(Stdio::from(
                File::create(runtime.join("race.attach.log")).unwrap(),
            ))
            .spawn()
            .unwrap();
        while !race_ready.exists() {
            assert!(
                !clock.expired(Instant::now()),
                "{provider} race attachment did not start"
            );
            assert!(
                race.try_wait().unwrap().is_none(),
                "{provider} race attachment closed early"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
        let control_deadline = Instant::now() + Duration::from_secs(5);
        let race_generation = loop {
            if let Ok(mut control_probe) = fluent::terminal_sessions::resolve(&project, &session)
                && control_probe.acquire_control().unwrap()
                    == fluent::terminal_sessions::ControlOutcome::Occupied
            {
                break control_probe.descriptor().execution_generation.clone();
            }
            assert!(
                Instant::now() < control_deadline && race.try_wait().unwrap().is_none(),
                "{provider} race attachment did not acquire Control"
            );
            std::thread::sleep(Duration::from_millis(25));
        };
        // Give the completed turn a full window after the race attachment
        // acquires Control, independently of time spent starting the Task.
        clock.begin_settlement(Instant::now());
        let before_race_sequence = loop {
            let transcript =
                fs::read_to_string(artifact.join("transcript.jsonl")).unwrap_or_default();
            if clock.expired(Instant::now()) {
                let recent_hooks: Vec<_> = transcript
                    .lines()
                    .rev()
                    .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                    .filter_map(|record| {
                        record["event"]["hook_event_name"]
                            .as_str()
                            .map(str::to_owned)
                    })
                    .take(5)
                    .collect();
                panic!(
                    "{provider} emitted no settling boundary after race attachment: recent_hooks={recent_hooks:?}; inspect {} and {}",
                    artifact.display(),
                    runtime.display()
                );
            }
            let boundary_seen = transcript
                .lines()
                .filter(|line| line.contains(boundary))
                .count()
                > previous_boundaries;
            if boundary_seen || child.child.try_wait().unwrap().is_some() {
                let last = transcript
                    .lines()
                    .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                    .filter_map(|record| record["input_sequence"].as_u64())
                    .max()
                    .unwrap_or(0);
                break last;
            }
            std::thread::sleep(Duration::from_millis(25));
        };
        fs::write(&race_release, b"send").unwrap();
        let race_deadline = Instant::now() + Duration::from_secs(70);
        let race_status = loop {
            if let Some(status) = race.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= race_deadline {
                let trace = fs::read_to_string(runtime.join("race.state")).unwrap_or_default();
                let attachment_pid = trace
                    .lines()
                    .find_map(|line| line.strip_prefix("attachment-pid=")?.parse::<u32>().ok());
                let mut diagnostic = format!("race state:\n{trace}\n");
                for pid in [Some(race.id()), attachment_pid].into_iter().flatten() {
                    if let Ok(output) = Command::new("ps")
                        .args(["-o", "pid,ppid,stat,command", "-p", &pid.to_string()])
                        .output()
                    {
                        diagnostic.push_str(&String::from_utf8_lossy(&output.stdout));
                    }
                }
                let _ = fs::write(runtime.join("race.timeout.txt"), diagnostic);
                race.kill().unwrap();
                let _ = race.wait();
                panic!(
                    "{provider} race attachment did not close; inspect {}",
                    runtime.join("race.timeout.txt").display()
                );
            }
            std::thread::sleep(Duration::from_millis(25));
        };
        assert!(
            race_status.success(),
            "{provider} race attachment failed; inspect {} and {}",
            runtime.join("race.attach.log").display(),
            runtime.join("race.state").display()
        );
        assert!(
            race_attempt.is_file(),
            "{provider} settlement input was never attempted"
        );
        // The attachment stays connected until EOF from this Writer terminal.
        // The descriptor can lag that EOF briefly, so observe its generation
        // closing before waiting for Task completion or a following worker.
        let closure_deadline = Instant::now() + Duration::from_secs(2);
        let closed_after_attempt = loop {
            if race_generation_closed_after_attempt(
                &race_attempt,
                &fluent::terminal_sessions::list(&project).unwrap(),
                &session,
                &race_generation,
            ) {
                break true;
            }
            if Instant::now() >= closure_deadline {
                break false;
            }
            std::thread::sleep(Duration::from_millis(25));
        };
        let status = loop {
            if let Some(status) = child.child.try_wait().unwrap() {
                break status;
            }
            if clock.expired(Instant::now()) {
                child.child.kill().unwrap();
                let _ = child.child.wait();
                panic!(
                    "{provider} native Task exceeded the host deadline; inspect {} and {}",
                    project.display(),
                    runtime.display()
                );
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        assert!(
            status.success(),
            "{provider} native Task failed; inspect {} and {}",
            project.display(),
            runtime.display()
        );
        if let Some(proxy) = &pi_proxy {
            assert!(
                proxy.output.join("retry-triggered.txt").is_file(),
                "Pi did not receive the transient retry response"
            );
            let count: usize = fs::read_to_string(proxy.output.join("request-count.txt"))
                .unwrap()
                .parse()
                .unwrap();
            assert!(count >= 2, "Pi did not retry the model request");
        }
        let transcript = fs::read_to_string(artifact.join("transcript.jsonl")).unwrap();
        let records: Vec<serde_json::Value> = transcript
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let writer_generation = records
            .iter()
            .find_map(|record| record["execution_generation"].as_str())
            .expect("native Writer has no execution generation");
        assert_eq!(
            race_generation, writer_generation,
            "{provider} race attachment targeted another Writer generation"
        );
        let raced_inputs: Vec<_> = records
            .iter()
            .filter(|record| {
                native_input_event(record)
                    && record["execution_generation"] == writer_generation
                    && record["input_sequence"]
                        .as_u64()
                        .is_some_and(|sequence| sequence > before_race_sequence)
                    && record["event"].to_string().contains(&race_marker)
            })
            .collect();
        let other_late_inputs = records.iter().any(|record| {
            native_input_event(record)
                && record["execution_generation"] == writer_generation
                && record["input_sequence"]
                    .as_u64()
                    .is_some_and(|sequence| sequence > before_race_sequence)
                && !record["event"].to_string().contains(&race_marker)
        });
        assert!(
            !other_late_inputs,
            "{provider} admitted an unidentified raced input"
        );
        assert!(
            raced_inputs.len() <= 1,
            "{provider} admitted raced input more than once"
        );
        let send_result = fs::read_to_string(&race_attempt).unwrap();
        assert!(
            send_result.starts_with("0 ") || send_result.starts_with("1 "),
            "{provider} did not record the settlement send result"
        );
        if send_result.starts_with("1 ") {
            assert!(
                raced_inputs.is_empty(),
                "{provider} admitted rejected input"
            );
        }
        if raced_inputs.is_empty() {
            let rejected_by_owner = records.iter().any(|record| {
                record["execution_generation"] == writer_generation
                    && record["event"]["type"] == "input_rejected"
                    && record["event"]["request"]
                        .to_string()
                        .contains(&race_marker)
            });
            let rejected_by_pi = provider == "pi"
                && fs::read_to_string(artifact.join("native-pi-rejected-input.jsonl"))
                    .unwrap_or_default()
                    .lines()
                    .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                    .any(|record| {
                        record["generation"] == writer_generation
                            && record["boundary"] == "pi-input"
                            && record["result"] == "rejected"
                            && record["text"] == race_marker
                    });
            assert!(
                rejected_by_owner || rejected_by_pi || closed_after_attempt,
                "{provider} raced input has no admission, native rejection, or observed closure of its generation after the send attempt"
            );
        }
        if let Some(input) = raced_inputs.first() {
            let sequence = input["input_sequence"].as_u64().unwrap();
            let consumed = records.iter().any(|record| {
                record["execution_generation"] == writer_generation
                    && record["input_sequence"] == sequence
                    && match provider {
                        "codex" => {
                            record["event"]["method"] == "turn/completed"
                                && record["event"]["params"]["turn"]["status"] == "completed"
                        }
                        "claude" => {
                            record["event"]["hook_event_name"] == "Stop"
                                && record["turn_id"] == input["turn_id"]
                        }
                        "pi" => record["event"]["type"] == "input_entry",
                        _ => false,
                    }
            });
            assert!(
                consumed,
                "{provider} admitted raced input without completing its turn"
            );
        }
        assert!(
            records
                .iter()
                .any(|record| record["format"] == "fluent-native-agent-event"
                    && record["provider"] == provider
                    && record["schema_version"] == 1),
            "{provider} has no canonical native evidence"
        );
        assert!(
            records
                .iter()
                .filter_map(|record| record["input_sequence"].as_u64())
                .max()
                .is_some_and(|sequence| sequence >= 3),
            "{provider} did not admit both attached inputs"
        );
        if provider == "codex" {
            let conversation = records
                .iter()
                .find(|record| record["execution_generation"] == writer_generation)
                .and_then(|record| record["assigned_conversation_id"].as_str())
                .expect("Codex Writer has no assigned conversation");
            let attached_sequences: Vec<_> = [
                "Please retain result.txt and finish the Task after committing.",
                "The final file must have exactly the requested line.",
            ]
            .into_iter()
            .map(|message| {
                records
                    .iter()
                    .find(|record| {
                        record["execution_generation"] == writer_generation
                            && record["assigned_conversation_id"] == conversation
                            && native_input_event(record)
                            && record["event"].to_string().contains(message)
                    })
                    .and_then(|record| record["input_sequence"].as_u64())
                    .unwrap_or_else(|| panic!("Codex did not admit attached input: {message}"))
            })
            .collect();
            assert!(
                attached_sequences[0] < attached_sequences[1],
                "Codex attached inputs were not admitted in order"
            );
            assert!(
                records.iter().any(|record| {
                    record["execution_generation"] == writer_generation
                        && record["assigned_conversation_id"] == conversation
                        && record["event"]["method"] == "turn/completed"
                        && record["event"]["params"]["turn"]["status"] == "completed"
                        && record["input_sequence"]
                            .as_u64()
                            .is_some_and(|sequence| sequence >= attached_sequences[1])
                }),
                "Codex settled before consuming both attached inputs"
            );
        }
        let shown = Command::new(binary)
            .args(["work-item", "show", &work])
            .current_dir(&project)
            .output()
            .unwrap();
        assert!(shown.status.success());
        let item: serde_json::Value = serde_json::from_slice(&shown.stdout).unwrap();
        let task = &item["attempts"][0]["tasks"][0];
        assert_eq!(task["status"], "complete");
        let workspace = project.join(task["output"]["workspace_path"].as_str().unwrap());
        assert_eq!(
            fs::read_to_string(workspace.join("result.txt")).unwrap(),
            format!("native-{provider}\n")
        );
        assert!(task["output"]["commit"].as_str().is_some());
        assert!(
            !forbidden.exists(),
            "{provider} escaped its candidate confinement"
        );
        assert!(
            transcript.contains("forbidden.txt"),
            "{provider} did not record its confinement probe"
        );
        if provider == "pi" {
            assert!(
                records
                    .iter()
                    .any(|record| record["event"]["type"] == "input"
                        && record["event"]["streamingBehavior"] == "steer")
            );
            assert!(
                records
                    .iter()
                    .any(|record| record["event"]["type"] == "input"
                        && record["event"]["streamingBehavior"] == "followUp")
            );
        }
        let listed = Command::new(binary)
            .args(["terminal", "list", "--source", project.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(listed.status.success());
        let listed: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
        assert!(
            !listed
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["session_id"] == session && entry["can_control"] == true),
            "settled {provider} terminal still admits control"
        );
        let late = Command::new(binary)
            .args([
                "terminal",
                "attach",
                "--source",
                project.to_str().unwrap(),
                &session,
                "--control",
            ])
            .output()
            .unwrap();
        assert!(
            !late.status.success(),
            "settled {provider} terminal accepted a late control attachment"
        );
        {
            run(binary, &["review", &work, "attempt-1"]);
            let review_log = File::create(runtime.join("review.stdout.log")).unwrap();
            let review_error = File::create(runtime.join("review.stderr.log")).unwrap();
            let mut review = Command::new(binary)
                .args(["task", "run", &work, "attempt-1", "attempt-1-review-tests"])
                .current_dir(&project)
                .stdout(Stdio::from(review_log))
                .stderr(Stdio::from(review_error))
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(600);
            loop {
                if let Some(status) = review.try_wait().unwrap() {
                    assert!(
                        status.success(),
                        "ephemeral native {provider} Reviewer failed; inspect {} and {}",
                        project.display(),
                        runtime.display()
                    );
                    break;
                }
                if Instant::now() >= deadline {
                    review.kill().unwrap();
                    let _ = review.wait();
                    panic!("ephemeral native {provider} Reviewer exceeded the host deadline");
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            let review_artifact = project.join(format!(
                ".fluent/work/artifacts/{work}/attempt-1/attempt-1-review-tests"
            ));
            assert!(review_artifact.join("review.md").is_file());
            let transcript = fs::read_to_string(review_artifact.join("transcript.jsonl")).unwrap();
            assert!(
                !transcript
                    .lines()
                    .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                    .any(|record| native_input_event(&record)
                        && record["input_sequence"]
                            .as_u64()
                            .is_some_and(|sequence| sequence > 1)),
                "{provider} late Writer input became live Reviewer input"
            );
            assert!(
                transcript
                    .lines()
                    .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                    .any(|record| record["format"] == "fluent-native-agent-event"
                        && record["provider"] == provider)
            );
            if provider == "codex" {
                assert!(
                    !review_artifact.join("codex-session").exists(),
                    "ephemeral Reviewer retained a resumable Codex session"
                );
            }
            let listed = Command::new(binary)
                .args(["terminal", "list", "--source", project.to_str().unwrap()])
                .output()
                .unwrap();
            let listed: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
            assert!(
                !listed
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|entry| entry["task_id"] == "attempt-1-review-tests"
                        && entry["can_control"] == true),
                "{provider} Reviewer remained controllable after settlement"
            );
        }

        let cancel_work = format!("native-{provider}-cancel");
        run(
            binary,
            &[
                "work-item",
                "create",
                &cancel_work,
                "--title",
                "Native cancellation",
                "--instructions",
                "Run sleep 60 with the shell tool before changing any files. The host will cancel this Attempt while the tool runs.",
            ],
        );
        run(
            binary,
            &[
                "attempt",
                "create",
                &cancel_work,
                "attempt-1",
                "--terminal-mode",
                "native",
                "--write-coder",
                provider,
                "--write-model",
                &model,
                "--write-effort",
                &effort,
            ],
        );
        let cancel_log = File::create(runtime.join("cancel.stdout.log")).unwrap();
        let cancel_error = File::create(runtime.join("cancel.stderr.log")).unwrap();
        let mut cancelled = Command::new(binary)
            .args([
                "task",
                "run",
                &cancel_work,
                "attempt-1",
                "attempt-1-write-1",
            ])
            .current_dir(&project)
            .stdout(Stdio::from(cancel_log))
            .stderr(Stdio::from(cancel_error))
            .spawn()
            .unwrap();
        let cancel_artifact = project.join(format!(
            ".fluent/work/artifacts/{cancel_work}/attempt-1/attempt-1-write-1/transcript.jsonl"
        ));
        let cancel_deadline = Instant::now() + Duration::from_secs(120);
        loop {
            assert!(
                cancelled.try_wait().unwrap().is_none(),
                "{provider} cancellation fixture ended before cancellation"
            );
            assert!(
                Instant::now() < cancel_deadline,
                "{provider} cancellation fixture did not start"
            );
            let transcript = fs::read_to_string(&cancel_artifact).unwrap_or_default();
            if transcript
                .lines()
                .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                .any(|record| {
                    record["input_sequence"]
                        .as_u64()
                        .is_some_and(|sequence| sequence >= 1)
                })
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        run(binary, &["attempt", "cancel", &cancel_work, "attempt-1"]);
        let cancel_deadline = Instant::now() + Duration::from_secs(20);
        let cancelled_status = loop {
            if let Some(status) = cancelled.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= cancel_deadline {
                cancelled.kill().unwrap();
                let _ = cancelled.wait();
                panic!("{provider} cancellation did not stop its Task runner");
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        assert!(
            !cancelled_status.success(),
            "{provider} cancelled Task reported success"
        );
        let shown = Command::new(binary)
            .args(["work-item", "show", &cancel_work])
            .current_dir(&project)
            .output()
            .unwrap();
        assert!(shown.status.success());
        let item: serde_json::Value = serde_json::from_slice(&shown.stdout).unwrap();
        assert_ne!(item["attempts"][0]["tasks"][0]["status"], "complete");

        if provider == "pi" {
            let failure_work = "native-pi-receiver-failure";
            run(
                binary,
                &[
                    "work-item",
                    "create",
                    failure_work,
                    "--title",
                    "Pi receiver failure",
                    "--instructions",
                    "Run sleep 60 with the shell tool before creating result.txt.",
                ],
            );
            run(
                binary,
                &[
                    "attempt",
                    "create",
                    failure_work,
                    "attempt-1",
                    "--terminal-mode",
                    "native",
                    "--write-coder",
                    "pi",
                    "--write-model",
                    &model,
                    "--write-effort",
                    &effort,
                ],
            );
            let failure_log = File::create(runtime.join("receiver.stdout.log")).unwrap();
            let failure_error = File::create(runtime.join("receiver.stderr.log")).unwrap();
            let mut failed = Command::new(binary)
                .args([
                    "task",
                    "run",
                    failure_work,
                    "attempt-1",
                    "attempt-1-write-1",
                ])
                .current_dir(&project)
                .env("FLUENT_NATIVE_PI_ENDPOINT", &pi_endpoint)
                .stdout(Stdio::from(failure_log))
                .stderr(Stdio::from(failure_error))
                .spawn()
                .unwrap();
            let failure_artifact = project.join(format!(
                ".fluent/work/artifacts/{failure_work}/attempt-1/attempt-1-write-1"
            ));
            let failure_deadline = Instant::now() + Duration::from_secs(120);
            loop {
                assert!(
                    failed.try_wait().unwrap().is_none(),
                    "Pi ended before receiver failure injection"
                );
                assert!(
                    Instant::now() < failure_deadline,
                    "Pi did not start receiver failure fixture"
                );
                let transcript = fs::read_to_string(failure_artifact.join("transcript.jsonl"))
                    .unwrap_or_default();
                if transcript
                    .lines()
                    .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                    .any(|event| event["event"]["type"] == "session_start")
                {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            let pane = fs::read_to_string(failure_artifact.join("native-pi-pane.sh")).unwrap();
            let (generation_file, generation) = pi_pane_generation(&pane);
            let socket = generation_file.parent().unwrap().join("pi.sock");
            let late_marker = "pi-input-after-receiver-loss";
            const LOST_RECEIVER_INPUT: &str = r#"
set timeout 10
spawn -noecho $env(FLUENT_NATIVE_BINARY) terminal attach --source $env(FLUENT_NATIVE_SOURCE) $env(FLUENT_NATIVE_SESSION) --control
expect -timeout 1 { eof { exit 1 } timeout {} }
set ready [open $env(FLUENT_NATIVE_LATE_READY) w]
puts $ready "attached"
close $ready
while {![file exists $env(FLUENT_NATIVE_LATE_RELEASE)]} { after 25 }
set result [catch {send -- "$env(FLUENT_NATIVE_LATE_TEXT)\r"} error]
set attempt [open $env(FLUENT_NATIVE_LATE_ATTEMPT) w]
puts $attempt "$result $error"
close $attempt
if {!$result} { after 100; catch {send -- "\002d"} }
expect eof
set outcome [wait]
exit [lindex $outcome 3]
"#;
            let listed = Command::new(binary)
                .args(["terminal", "list", "--source", project.to_str().unwrap()])
                .output()
                .unwrap();
            let listed: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
            let session = listed
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["work_item_id"] == failure_work && entry["can_control"] == true)
                .and_then(|entry| entry["session_id"].as_str())
                .expect("Pi receiver-loss fixture had no live controllable terminal");
            let late_ready = runtime.join("receiver-attach.ready");
            let late_release = runtime.join("receiver-attach.release");
            let late_attempt = runtime.join("receiver-attach.attempt");
            let mut attachment = Command::new("expect")
                .args(["-c", LOST_RECEIVER_INPUT])
                .env("FLUENT_NATIVE_BINARY", binary)
                .env("FLUENT_NATIVE_SOURCE", &project)
                .env("FLUENT_NATIVE_SESSION", session)
                .env("FLUENT_NATIVE_LATE_TEXT", late_marker)
                .env("FLUENT_NATIVE_LATE_READY", &late_ready)
                .env("FLUENT_NATIVE_LATE_RELEASE", &late_release)
                .env("FLUENT_NATIVE_LATE_ATTEMPT", &late_attempt)
                .stdout(Stdio::from(
                    File::create(runtime.join("receiver-attach.log")).unwrap(),
                ))
                .spawn()
                .unwrap();
            while !late_ready.exists() {
                assert!(
                    Instant::now() < failure_deadline,
                    "Pi control did not attach"
                );
                assert!(
                    attachment.try_wait().unwrap().is_none(),
                    "Pi control closed before receiver loss"
                );
                std::thread::sleep(Duration::from_millis(25));
            }
            let control_deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Ok(mut control_probe) = fluent::terminal_sessions::resolve(&project, session)
                    && control_probe.acquire_control().unwrap()
                        == fluent::terminal_sessions::ControlOutcome::Occupied
                {
                    break;
                }
                assert!(
                    Instant::now() < control_deadline && attachment.try_wait().unwrap().is_none(),
                    "Pi receiver-loss attachment did not acquire Control"
                );
                std::thread::sleep(Duration::from_millis(25));
            }
            fs::remove_file(&socket).unwrap();
            fs::write(&late_release, b"send").unwrap();
            while attachment.try_wait().unwrap().is_none() {
                assert!(
                    Instant::now() < failure_deadline,
                    "Pi late control did not close"
                );
                std::thread::sleep(Duration::from_millis(25));
            }
            assert!(late_attempt.is_file(), "Pi late input was never attempted");
            let send_result = fs::read_to_string(&late_attempt).unwrap();
            assert!(
                send_result.starts_with("0 ") || send_result.starts_with("1 "),
                "Pi did not record the late input send result"
            );
            let failure_deadline = Instant::now() + Duration::from_secs(20);
            let status = loop {
                if let Some(status) = failed.try_wait().unwrap() {
                    break status;
                }
                if Instant::now() >= failure_deadline {
                    failed.kill().unwrap();
                    let _ = failed.wait();
                    panic!("Pi did not stop after owner receiver disappeared");
                }
                std::thread::sleep(Duration::from_millis(100));
            };
            assert!(
                !status.success(),
                "Pi receiver failure reported Task success"
            );
            let rejection =
                fs::read_to_string(failure_artifact.join("native-pi-rejected-input.jsonl"))
                    .expect("Pi did not record rejection at its input handler");
            assert!(
                rejection.lines().any(|line| {
                    let record: serde_json::Value = serde_json::from_str(line).unwrap();
                    record["generation"] == generation
                        && record["boundary"] == "pi-input"
                        && record["result"] == "rejected"
                        && record["text"] == late_marker
                }),
                "Pi receiver-loss marker has no generation-bound input rejection"
            );
            let shown = Command::new(binary)
                .args(["work-item", "show", failure_work])
                .current_dir(&project)
                .output()
                .unwrap();
            let item: serde_json::Value = serde_json::from_slice(&shown.stdout).unwrap();
            assert_ne!(item["attempts"][0]["tasks"][0]["status"], "complete");
            let failed_transcript =
                fs::read_to_string(failure_artifact.join("transcript.jsonl")).unwrap_or_default();
            assert!(
                !failed_transcript.contains(late_marker),
                "Pi accepted input after its owner receiver disappeared"
            );
            assert!(
                !failure_artifact.join("pi-session.jsonl").exists(),
                "Pi copied a successful session after receiver failure"
            );
            assert!(
                !pi_proxy
                    .as_ref()
                    .unwrap()
                    .output
                    .join("rejected-seen.txt")
                    .exists(),
                "Pi modeled input that its owner receiver could not admit"
            );

            for (scenario, input) in [("abort", "\u{1b}"), ("compact", "/compact\r")] {
                let abort_work = format!("native-pi-{scenario}");
                run(
                    binary,
                    &[
                        "work-item",
                        "create",
                        &abort_work,
                        "--title",
                        "Pi activity interruption",
                        "--instructions",
                        "Run sleep 60 with the shell tool before creating result.txt.",
                    ],
                );
                run(
                    binary,
                    &[
                        "attempt",
                        "create",
                        &abort_work,
                        "attempt-1",
                        "--terminal-mode",
                        "native",
                        "--write-coder",
                        "pi",
                        "--write-model",
                        &model,
                        "--write-effort",
                        &effort,
                    ],
                );
                let abort_log =
                    File::create(runtime.join(format!("{scenario}.stdout.log"))).unwrap();
                let abort_error =
                    File::create(runtime.join(format!("{scenario}.stderr.log"))).unwrap();
                let mut aborted = Command::new(binary)
                    .args(["task", "run", &abort_work, "attempt-1", "attempt-1-write-1"])
                    .current_dir(&project)
                    .stdout(Stdio::from(abort_log))
                    .stderr(Stdio::from(abort_error))
                    .spawn()
                    .unwrap();
                let abort_artifact = project.join(format!(
                    ".fluent/work/artifacts/{abort_work}/attempt-1/attempt-1-write-1"
                ));
                let abort_deadline = Instant::now() + Duration::from_secs(120);
                let abort_session = loop {
                    assert!(
                        aborted.try_wait().unwrap().is_none(),
                        "Pi ended before abort injection"
                    );
                    assert!(
                        Instant::now() < abort_deadline,
                        "Pi did not start abort fixture"
                    );
                    let listed = Command::new(binary)
                        .args(["terminal", "list", "--source", project.to_str().unwrap()])
                        .output()
                        .unwrap();
                    let listed: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
                    if let Some(session) = listed
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|entry| {
                            entry["work_item_id"] == abort_work && entry["can_control"] == true
                        })
                        .and_then(|entry| entry["session_id"].as_str())
                    {
                        let transcript =
                            fs::read_to_string(abort_artifact.join("transcript.jsonl"))
                                .unwrap_or_default();
                        if transcript.contains("tool_execution_start") {
                            break session.to_owned();
                        }
                    }
                    std::thread::sleep(Duration::from_millis(100));
                };
                let abort_attach = r#"
set timeout 20
spawn -noecho $env(FLUENT_NATIVE_BINARY) terminal attach --source $env(FLUENT_NATIVE_SOURCE) $env(FLUENT_NATIVE_SESSION) --control
after 300
send -- $env(FLUENT_NATIVE_FAULT_INPUT)
after 300
send -- "\002d"
expect eof
set outcome [wait]
exit [lindex $outcome 3]
"#;
                let attach = Command::new("expect")
                    .args(["-c", abort_attach])
                    .env("FLUENT_NATIVE_BINARY", binary)
                    .env("FLUENT_NATIVE_SOURCE", &project)
                    .env("FLUENT_NATIVE_SESSION", &abort_session)
                    .env("FLUENT_NATIVE_FAULT_INPUT", input)
                    .output()
                    .unwrap();
                fs::write(
                    runtime.join(format!("{scenario}.attach.log")),
                    &attach.stdout,
                )
                .unwrap();
                let abort_deadline = Instant::now() + Duration::from_secs(20);
                let status = loop {
                    if let Some(status) = aborted.try_wait().unwrap() {
                        break status;
                    }
                    if Instant::now() >= abort_deadline {
                        aborted.kill().unwrap();
                        let _ = aborted.wait();
                        panic!("Pi {scenario} did not stop its Task runner");
                    }
                    std::thread::sleep(Duration::from_millis(100));
                };
                assert!(
                    !status.success(),
                    "Pi {scenario} activity completed the Task"
                );
                let transcript =
                    fs::read_to_string(abort_artifact.join("transcript.jsonl")).unwrap();
                assert!(
                    transcript.contains("\"outcome\":\"aborted\"")
                        || transcript.contains("\"aborted\":true"),
                    "Pi {scenario} has no canonical interruption evidence"
                );
            }
        }
        child.armed = false;
    }
}

const NATIVE_FIXTURE_GIT_EMAIL: &str = "native-fixture@example.com";

fn install_native_fixture_tester(root: &std::path::Path) {
    use std::fs;

    fs::create_dir_all(root.join(".fluent")).unwrap();
    fs::write(
        root.join(".fluent/tester.yaml"),
        "commands:\n  - command: 'true'\n    test_harness: shell-harness\n",
    )
    .unwrap();
    let extractor = root.join(".fluent/extract-tester-results");
    fs::write(&extractor, "#!/bin/sh\nprintf '[]\\n'\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&extractor, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(fs::metadata(&extractor).unwrap().permissions().mode() & 0o111 != 0);
    }
    assert!(
        serde_yaml::from_str::<serde_yaml::Value>(
            &fs::read_to_string(root.join(".fluent/tester.yaml")).unwrap()
        )
        .unwrap()["commands"]
            .as_sequence()
            .is_some_and(|commands| !commands.is_empty()),
        "attached fixture requires a declared Tester command"
    );
}

#[test]
fn native_progression_fixture_declares_tester() {
    assert!(
        !NATIVE_FIXTURE_GIT_EMAIL.trim().is_empty()
            && !NATIVE_FIXTURE_GIT_EMAIL.ends_with(".invalid"),
        "native fixture commits must pass Fluent's metadata gate"
    );
    let root = tempfile::tempdir().unwrap();
    install_native_fixture_tester(root.path());
    let output = std::process::Command::new(root.path().join(".fluent/extract-tester-results"))
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"[]\n");
}

/// Keep an observer connected while the Attempt owner moves from its Writer
/// into Review. The attachment itself never sends a detach request.
#[test]
#[ignore]
fn host_native_attached_attempt_progression() {
    use std::fs::{self, File};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    assert_eq!(
        std::env::var("FLUENT_TEST_REAL_NATIVE_AGENT_HOST").unwrap(),
        "1"
    );
    let root = std::path::PathBuf::from(std::env::var("FLUENT_NATIVE_HOST_OUTPUT_DIR").unwrap())
        .join("attached-progression");
    // Review, Learning, and landing also require a clean source checkout.
    let runtime = root.with_file_name("attached-progression-runtime");
    fs::create_dir(&root).unwrap();
    fs::create_dir(&runtime).unwrap();
    let binary = env!("CARGO_BIN_EXE_fluent");
    let run = |program: &str, args: &[&str]| {
        let output = Command::new(program)
            .args(args)
            .current_dir(&root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{program} {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    run("git", &["init", "-b", "main"]);
    run("git", &["config", "user.name", "Native Fixture"]);
    run("git", &["config", "user.email", NATIVE_FIXTURE_GIT_EMAIL]);
    run("git", &["config", "commit.gpgsign", "false"]);
    fs::write(root.join("README.md"), "Native progression fixture.\n").unwrap();
    install_native_fixture_tester(&root);
    run("git", &["add", "README.md"]);
    run(
        "git",
        &[
            "add",
            "-f",
            ".fluent/tester.yaml",
            ".fluent/extract-tester-results",
        ],
    );
    run("git", &["commit", "-m", "Add fixture"]);
    let work = "native-attached-progression";
    run(
        binary,
        &[
            "work-item",
            "create",
            work,
            "--title",
            "Attached progression",
            "--instructions",
            "Run sleep 8 with the shell tool, then replace README.md with exactly `Native progression candidate.` followed by a newline. Commit the change. Keep it small.",
        ],
    );
    let model = std::env::var("FLUENT_NATIVE_CODEX_MODEL").unwrap();
    let effort = std::env::var("FLUENT_NATIVE_CODEX_EFFORT").unwrap();
    run(
        binary,
        &[
            "attempt",
            "create",
            work,
            "attempt-1",
            "--terminal-mode",
            "native",
            "--write-coder",
            "codex",
            "--write-model",
            &model,
            "--write-effort",
            &effort,
            "--review-coder",
            "codex",
            "--review-model",
            &model,
            "--review-effort",
            &effort,
        ],
    );
    let mut owner = Command::new(binary)
        .args(["attempt", "run", work, "attempt-1"])
        .current_dir(&root)
        .stdout(Stdio::from(
            File::create(runtime.join("attempt.stdout.log")).unwrap(),
        ))
        .stderr(Stdio::from(
            File::create(runtime.join("attempt.stderr.log")).unwrap(),
        ))
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(600);
    let session = loop {
        assert!(
            owner.try_wait().unwrap().is_none(),
            "Attempt ended before native attachment"
        );
        assert!(
            Instant::now() < deadline,
            "Writer did not expose its terminal"
        );
        let listed = Command::new(binary)
            .args(["terminal", "list", "--source", root.to_str().unwrap()])
            .output()
            .unwrap();
        if listed.status.success()
            && let Ok(value) = serde_json::from_slice::<serde_json::Value>(&listed.stdout)
            && let Some(id) = value
                .as_array()
                .and_then(|items| {
                    items.iter().find(|entry| {
                        entry["task_id"] == "attempt-1-write-1" && entry["can_observe"] == true
                    })
                })
                .and_then(|entry| entry["session_id"].as_str())
        {
            break id.to_owned();
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    const HOLD_ATTACHMENT: &str = r#"
set timeout 600
spawn -noecho $env(FLUENT_NATIVE_BINARY) terminal attach --source $env(FLUENT_NATIVE_SOURCE) $env(FLUENT_NATIVE_SESSION)
after 500
set marker [open $env(FLUENT_NATIVE_READY) w]
puts $marker "attached"
close $marker
expect eof
set outcome [wait]
exit [lindex $outcome 3]
"#;
    let marker = runtime.join("observer.ready");
    let mut observer = Command::new("expect")
        .args(["-c", HOLD_ATTACHMENT])
        .env("FLUENT_NATIVE_BINARY", binary)
        .env("FLUENT_NATIVE_SOURCE", &root)
        .env("FLUENT_NATIVE_SESSION", &session)
        .env("FLUENT_NATIVE_READY", &marker)
        .stdout(Stdio::from(
            File::create(runtime.join("observer.log")).unwrap(),
        ))
        .spawn()
        .unwrap();
    while !marker.exists() {
        assert!(Instant::now() < deadline, "observer did not attach");
        assert!(
            observer.try_wait().unwrap().is_none(),
            "observer exited before attachment"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let mut saw_writer_complete = false;
    let mut saw_review = false;
    let mut held_control_during_writer = false;
    while Instant::now() < deadline {
        let shown = Command::new(binary)
            .args(["work-item", "show", work])
            .current_dir(&root)
            .output()
            .unwrap();
        if shown.status.success()
            && let Ok(item) = serde_json::from_slice::<serde_json::Value>(&shown.stdout)
            && let Some(tasks) = item["attempts"][0]["tasks"].as_array()
        {
            held_control_during_writer |= observer.try_wait().unwrap().is_none()
                && tasks
                    .iter()
                    .any(|task| task["id"] == "attempt-1-write-1" && task["status"] == "executing");
            saw_writer_complete |= tasks
                .iter()
                .any(|task| task["id"] == "attempt-1-write-1" && task["status"] == "complete");
            saw_review |= saw_writer_complete
                && tasks.iter().any(|task| {
                    task["kind"] == "review"
                        && matches!(task["status"].as_str(), Some("executing" | "complete"))
                });
            if saw_review {
                break;
            }
        }
        if owner.try_wait().unwrap().is_some() && !saw_review {
            panic!("Attempt stopped before automatic Review progression");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(saw_writer_complete, "attached Writer did not complete");
    assert!(
        held_control_during_writer,
        "Control was not held during the Writer Task"
    );
    assert!(
        saw_review,
        "Attempt did not start Review after attached Writer settled"
    );
    let observer_deadline = Instant::now() + Duration::from_secs(20);
    while observer.try_wait().unwrap().is_none() {
        if Instant::now() >= observer_deadline {
            observer.kill().unwrap();
            let _ = observer.wait();
            panic!("attached observer did not close with the settled Writer");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let transcript = root.join(format!(
        ".fluent/work/artifacts/{work}/attempt-1/attempt-1-write-1/transcript.jsonl"
    ));
    assert!(
        fs::read_to_string(transcript)
            .unwrap()
            .contains("fluent-native-agent-event")
    );
    let status = loop {
        if let Some(status) = owner.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            owner.kill().unwrap();
            let _ = owner.wait();
            panic!("attached Attempt did not finish its normal gates");
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(
        status.success(),
        "attached Attempt failed; inspect {} and {}",
        root.display(),
        runtime.display()
    );
    let shown = Command::new(binary)
        .args(["work-item", "show", work])
        .current_dir(&root)
        .output()
        .unwrap();
    let item: serde_json::Value = serde_json::from_slice(&shown.stdout).unwrap();
    assert_eq!(
        item["attempts"][0]["learning"]["status"], "succeeded",
        "native Learner did not pass candidate validation"
    );
    let tasks = item["attempts"][0]["tasks"].as_array().unwrap();
    let learner = tasks
        .iter()
        .find(|task| task["kind"] == "learn" && task["status"] == "complete")
        .expect("native Learner Task did not complete");
    let learner_area = root.join(learner["artifact_area"]["path"].as_str().unwrap());
    assert!(
        fs::read_to_string(learner_area.join("transcript.jsonl"))
            .unwrap()
            .contains("fluent-native-agent-event")
    );
    assert!(
        !learner_area.join("codex-session").exists(),
        "non-Writer Codex Learner retained a resumable session"
    );

    // Diverge main after the candidate is ready so landing must execute the
    // assigned native Rebase Task in this disposable fixture.
    fs::write(root.join("README.md"), "Native progression source.\n").unwrap();
    run("git", &["add", "README.md"]);
    run("git", &["commit", "-m", "Update source fixture"]);
    let mut land = Command::new(binary)
        .args([
            "merge-candidate",
            "land",
            work,
            "--coder",
            "codex",
            "--model",
            &model,
            "--effort",
            &effort,
        ])
        .current_dir(&root)
        .stdout(Stdio::from(
            File::create(runtime.join("land.stdout.log")).unwrap(),
        ))
        .stderr(Stdio::from(
            File::create(runtime.join("land.stderr.log")).unwrap(),
        ))
        .spawn()
        .unwrap();
    let land_deadline = Instant::now() + Duration::from_secs(600);
    let land_status = loop {
        if let Some(status) = land.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= land_deadline {
            land.kill().unwrap();
            let _ = land.wait();
            panic!("native Rebase did not finish in the disposable fixture");
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(
        land_status.success(),
        "native Rebase failed; inspect {} and {}",
        root.display(),
        runtime.display()
    );
    let shown = Command::new(binary)
        .args(["work-item", "show", work])
        .current_dir(&root)
        .output()
        .unwrap();
    let item: serde_json::Value = serde_json::from_slice(&shown.stdout).unwrap();
    let rebase = item["attempts"][0]["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|task| task["kind"] == "rebase" && task["status"] == "complete")
        .expect("native Rebase Task did not complete");
    let rebase_area = root.join(rebase["artifact_area"]["path"].as_str().unwrap());
    assert!(
        fs::read_to_string(rebase_area.join("transcript.jsonl"))
            .unwrap()
            .contains("fluent-native-agent-event")
    );
    assert!(
        !rebase_area.join("codex-session").exists(),
        "non-Writer Codex Rebase retained a resumable session"
    );
}

/// Run two native Writer Tasks through Fluent's Task launcher. The second Task
/// must resume the first Task's saved Codex conversation in a new terminal.
#[test]
fn native_continuation_fixtures_use_distinct_work_items() {
    assert_eq!(
        native_continuation_work_id("writer-retry"),
        "native-writer-retry"
    );
    assert_eq!(
        native_continuation_work_id("writer-continuation"),
        "native-writer-continuation"
    );
}

#[test]
#[ignore]
fn host_native_task_continuation_reuses_saved_conversation() {
    run_host_native_task_continuation("writer-continuation");
}

fn native_continuation_work_id(fixture_name: &str) -> String {
    format!("native-{fixture_name}")
}

fn run_host_native_task_continuation(fixture_name: &str) {
    use fluent::work_model::WorkModelStore;
    use std::fs;
    use std::process::Command;

    assert_eq!(
        std::env::var("FLUENT_TEST_REAL_NATIVE_AGENT_HOST").unwrap(),
        "1"
    );
    let root = std::path::PathBuf::from(std::env::var("FLUENT_NATIVE_HOST_OUTPUT_DIR").unwrap())
        .join(fixture_name);
    fs::create_dir(&root).unwrap();
    let binary = env!("CARGO_BIN_EXE_fluent");
    let run = |program: &str, args: &[&str]| {
        let output = Command::new(program)
            .args(args)
            .current_dir(&root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{program} {args:?} failed; stdout: {}; stderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    };
    run("git", &["init", "-b", "main"]);
    run("git", &["config", "user.name", "Native Fixture"]);
    run("git", &["config", "user.email", NATIVE_FIXTURE_GIT_EMAIL]);
    run("git", &["config", "commit.gpgsign", "false"]);
    fs::write(root.join("README.md"), "Native continuation fixture.\n").unwrap();
    run("git", &["add", "README.md"]);
    run("git", &["commit", "-m", "Add fixture"]);
    let work_id = native_continuation_work_id(fixture_name);
    let work = work_id.as_str();
    run(
        binary,
        &[
            "work-item",
            "create",
            work,
            "--title",
            "Native continuation",
            "--instructions",
            "In the assigned candidate workspace, write stage.txt as `first` followed by a newline if it does not exist; otherwise replace it with `second` followed by a newline. Commit the change. Complete this Task without modifying any other product file.",
        ],
    );
    let model = std::env::var("FLUENT_NATIVE_CODEX_MODEL").unwrap();
    let effort = std::env::var("FLUENT_NATIVE_CODEX_EFFORT").unwrap();
    run(
        binary,
        &[
            "attempt",
            "create",
            work,
            "attempt-1",
            "--terminal-mode",
            "native",
            "--write-coder",
            "codex",
            "--write-model",
            &model,
            "--write-effort",
            &effort,
        ],
    );
    run(
        binary,
        &["task", "run", work, "attempt-1", "attempt-1-write-1"],
    );
    let store = WorkModelStore::new(&root);
    let first = store.read_work_item(work).unwrap();
    let first_area = root.join(
        first.attempts[0].tasks[0]
            .artifact_area
            .as_ref()
            .unwrap()
            .path
            .as_str(),
    );
    assert!(
        first_area.join("codex-session").is_dir(),
        "first native Writer did not save its Codex session"
    );
    let read_bootstrap = |area: &std::path::Path| -> serde_json::Value {
        fs::read_to_string(area.join("transcript.jsonl"))
            .unwrap()
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .find(|record| {
                record["provider"] == "codex"
                    && record["input_sequence"] == 0
                    && record["assigned_conversation_id"].as_str().is_some()
            })
            .expect("native Task has no Codex bootstrap evidence")
    };
    let initial = read_bootstrap(&first_area);
    let saved = initial["assigned_conversation_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let commit = first.attempts[0].tasks[0]
        .output
        .as_ref()
        .unwrap()
        .commit
        .clone();
    // Task run deliberately stops before the Attempt loop records its Writer
    // result. Recreate that loop's saved continuation record from real Task
    // evidence, then launch the next Task through the normal Task command.
    store
        .mutate_work_item(work, |item| {
            item.attempts[0]
                .writer_runs
                .push(fluent::work_model::WriterRun {
                    task_id: "attempt-1-write-1".into(),
                    outcome: fluent::work_model::WriterOutcome::Continue,
                    kind: fluent::work_model::WriterRunKind::Initial,
                    provider: "codex".into(),
                    session_id: Some(saved.clone()),
                    continuation: 0,
                    checked_required: 0,
                    completed_matrix: 0,
                    candidate_commit: commit.clone(),
                });
            item.add_next_write_round("attempt-1", vec![])
        })
        .unwrap();
    let second_run = Command::new(binary)
        .args(["task", "run", work, "attempt-1", "attempt-1-write-2"])
        .current_dir(&root)
        .output()
        .unwrap();
    if !second_run.status.success() {
        let output = format!(
            "{}{}",
            String::from_utf8_lossy(&second_run.stdout),
            String::from_utf8_lossy(&second_run.stderr)
        );
        assert!(
            output.contains("Writer completion evidence is missing or invalid")
                && output.contains("native handoff correction requires a new execution generation"),
            "second native Writer failed before the expected post-turn handoff: {output}"
        );
    }
    let second = store.read_work_item(work).unwrap();
    let second_task = second.attempts[0]
        .tasks
        .iter()
        .find(|task| task.id == "attempt-1-write-2")
        .unwrap();
    if second_run.status.success() {
        assert_eq!(second_task.status, fluent::work_model::TaskStatus::Complete);
    }
    let second_area = root.join(second_task.artifact_area.as_ref().unwrap().path.as_str());
    let resumed = read_bootstrap(&second_area);
    let completed_turn = fs::read_to_string(second_area.join("transcript.jsonl"))
        .unwrap()
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .any(|record| {
            record["provider"] == "codex"
                && record["event"]["method"] == "turn/completed"
                && record["event"]["params"]["turn"]["status"] == "completed"
                && record["assigned_conversation_id"] == saved
                && record["execution_generation"] == resumed["execution_generation"]
        });
    assert!(
        completed_turn,
        "resumed native Writer did not complete a turn"
    );
    assert_eq!(initial["launch_method"], "thread/start");
    assert_eq!(resumed["launch_method"], "thread/resume");
    assert_eq!(initial["assigned_conversation_id"], saved);
    assert_eq!(resumed["assigned_conversation_id"], saved);
    assert!(
        initial["execution_generation"]
            .as_str()
            .is_some_and(|id| !id.is_empty())
    );
    assert!(
        resumed["execution_generation"]
            .as_str()
            .is_some_and(|id| !id.is_empty())
    );
    assert_ne!(
        initial["execution_generation"],
        resumed["execution_generation"]
    );
    assert_ne!(first.attempts[0].tasks[0].id, second_task.id);
}
