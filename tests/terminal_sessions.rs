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
    let base = std::fs::read_to_string(&profile.path).unwrap();
    let added = content.strip_prefix(&base).unwrap();
    assert!(added.contains(&format!("(subpath \"{}\")", socket_dir.display())));
    assert!(!added.contains("(subpath \"/private/tmp\")"));
}
