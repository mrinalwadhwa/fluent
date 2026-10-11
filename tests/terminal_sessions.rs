#![cfg(feature = "test-support")]

use fluent::terminal_sessions::{TerminalIdentity, TerminalSession};

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
