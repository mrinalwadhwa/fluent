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
assert args[:2] == ['fluent', 'agent-source']
os.execv({binary:?}, [{binary:?}] + args[1:])
"#,
        binary = binary.to_string_lossy()
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
