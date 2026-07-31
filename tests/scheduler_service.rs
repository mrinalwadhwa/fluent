use fluent::scheduler_service::{self, BuildIdentity, FakeServiceManager, ServiceManager};
use std::fs;
use std::io::{Read, Write};
use std::path::PathBuf;
use tempfile::TempDir;

// ─────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────

fn make_fake_exe(dir: &std::path::Path) -> PathBuf {
    let path = dir.join("fake-fluent");
    fs::write(&path, b"#!/bin/sh\necho fluent\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    path
}

fn fake_build() -> BuildIdentity {
    BuildIdentity {
        version: "0.1.0-test".to_string(),
        hash: "abc123def456".to_string(),
    }
}

fn make_event(seq: u64, attempt_id: &str, kind: &str) -> fluent::scheduler_service::AttemptEvent {
    fluent::scheduler_service::AttemptEvent {
        seq,
        kind: kind.to_string(),
        attempt_id: attempt_id.to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        detail: None,
    }
}

// ─────────────────────────────────────────────────
// Step 1: isolated-home setup — identity, registry, socket path, state root
//
// Note on socket health exchange: Unix domain socket creation is blocked by
// the execution environment sandbox (EPERM on bind). The protocol, framing,
// FakeSocketListener, and send_health_request are implemented in the module
// and exercised by code review. See Untestable note in progress.md.
// ─────────────────────────────────────────────────

#[test]
fn checkout_identity_is_assigned_and_stable() {
    let project = TempDir::new().unwrap();
    let id1 = scheduler_service::assign_checkout_identity(project.path()).unwrap();
    assert!(!id1.0.is_empty(), "identity must be non-empty");

    let id2 = scheduler_service::assign_checkout_identity(project.path()).unwrap();
    assert_eq!(id1, id2, "identity must be stable across calls");
}

#[test]
fn checkout_identity_differs_across_projects() {
    let p1 = TempDir::new().unwrap();
    let p2 = TempDir::new().unwrap();
    let id1 = scheduler_service::assign_checkout_identity(p1.path()).unwrap();
    let id2 = scheduler_service::assign_checkout_identity(p2.path()).unwrap();
    assert_ne!(
        id1, id2,
        "different checkouts must get different identities"
    );
}

#[test]
fn checkout_is_registered_and_read_back() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();

    let identity = scheduler_service::assign_checkout_identity(project.path()).unwrap();
    scheduler_service::register_checkout(home.path(), project.path(), &identity).unwrap();

    let registry = scheduler_service::read_registry(home.path()).unwrap();
    let canonical = project.path().canonicalize().unwrap();
    assert_eq!(
        registry.get(&canonical).unwrap(),
        &identity,
        "registered identity must be retrievable"
    );
}

#[test]
fn registration_is_atomic_and_overwrites_stale_entry() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();

    let id1 = scheduler_service::assign_checkout_identity(project.path()).unwrap();
    scheduler_service::register_checkout(home.path(), project.path(), &id1).unwrap();

    let id2 = fluent::scheduler_service::CheckoutIdentity("newidentity".to_string());
    scheduler_service::register_checkout(home.path(), project.path(), &id2).unwrap();

    let registry = scheduler_service::read_registry(home.path()).unwrap();
    let canonical = project.path().canonicalize().unwrap();
    assert_eq!(registry.get(&canonical).unwrap(), &id2);
}

#[test]
#[cfg(unix)]
fn service_state_root_is_user_private() {
    use std::os::unix::fs::PermissionsExt;

    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let identity = scheduler_service::assign_checkout_identity(project.path()).unwrap();
    scheduler_service::register_checkout(home.path(), project.path(), &identity).unwrap();

    let root = scheduler_service::service_state_root(home.path());
    let meta = fs::metadata(&root).unwrap();
    let mode = meta.permissions().mode() & 0o777;
    assert_eq!(
        mode, 0o700,
        "state root must be accessible only by the owner"
    );
}

#[test]
fn service_state_root_is_under_home_not_system_wide() {
    let home = TempDir::new().unwrap();
    let root = scheduler_service::service_state_root(home.path());
    assert!(
        root.starts_with(home.path()),
        "state root must be confined to the user home directory"
    );
}

#[test]
fn socket_path_is_within_user_private_state_root() {
    let home = TempDir::new().unwrap();
    let sock = scheduler_service::socket_path(home.path());
    let root = scheduler_service::service_state_root(home.path());
    assert!(
        sock.starts_with(&root),
        "socket path must be under the user-private state root, not a system-wide path"
    );
}

#[test]
fn fake_service_manager_starts_as_not_installed() {
    let manager = FakeServiceManager::new();
    assert!(!manager.is_installed());
    assert!(!manager.is_enabled());
    assert!(!manager.is_running());
}

#[test]
fn fake_service_manager_install_records_build_and_socket() {
    let home = TempDir::new().unwrap();
    let build = fake_build();
    let sock = scheduler_service::socket_path(home.path());
    let manager = FakeServiceManager::new();

    manager
        .install_or_update(&PathBuf::from("/fake/exe"), &build, &sock)
        .unwrap();

    assert!(manager.is_installed());
    assert_eq!(manager.installed_build().as_ref(), Some(&build));
    assert_eq!(manager.installed_sock_path().as_ref(), Some(&sock));
}

// ─────────────────────────────────────────────────
// Step 2: atomic executable staging, build state, lifecycle transitions
// ─────────────────────────────────────────────────

#[test]
fn executable_is_staged_atomically_and_returns_build_identity() {
    let home = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let exe = make_fake_exe(src.path());

    let build = scheduler_service::stage_executable(&exe, home.path()).unwrap();
    assert!(!build.version.is_empty(), "version must be non-empty");
    assert_eq!(build.hash.len(), 64, "SHA-256 hash must be 64 hex chars");

    let staged = scheduler_service::managed_executable_path(home.path());
    assert!(staged.exists(), "staged executable must exist");
    assert_eq!(
        fs::read(&staged).unwrap(),
        fs::read(&exe).unwrap(),
        "staged content must match source"
    );
}

#[test]
fn desired_and_observed_build_identities_are_recorded_and_read() {
    let home = TempDir::new().unwrap();
    let build = fake_build();

    assert!(
        scheduler_service::read_desired_build(home.path())
            .unwrap()
            .is_none()
    );
    assert!(
        scheduler_service::read_observed_build(home.path())
            .unwrap()
            .is_none()
    );

    scheduler_service::record_desired_build(home.path(), &build).unwrap();
    scheduler_service::record_observed_build(home.path(), &build).unwrap();

    assert_eq!(
        scheduler_service::read_desired_build(home.path())
            .unwrap()
            .as_ref(),
        Some(&build)
    );
    assert_eq!(
        scheduler_service::read_observed_build(home.path())
            .unwrap()
            .as_ref(),
        Some(&build)
    );
}

#[test]
fn service_manager_install_then_enable_disable_drain_transitions() {
    let home = TempDir::new().unwrap();
    let build = fake_build();
    let sock = scheduler_service::socket_path(home.path());
    let manager = FakeServiceManager::new();

    assert!(!manager.is_installed());

    manager
        .install_or_update(&PathBuf::from("/fake/exe"), &build, &sock)
        .unwrap();
    assert!(manager.is_installed());

    manager.enable().unwrap();
    assert!(manager.is_enabled());

    manager.start().unwrap();
    assert!(manager.is_running());

    manager.disable().unwrap();
    assert!(!manager.is_enabled(), "disabled must clear enabled flag");
    assert!(
        manager.is_running(),
        "disable must not stop the running service"
    );

    manager.drain().unwrap();
    assert!(!manager.is_running(), "drain must stop the service");
}

#[test]
fn enable_requires_prior_install() {
    let manager = FakeServiceManager::new();
    assert!(
        manager.enable().is_err(),
        "enable without install must fail"
    );
}

#[test]
fn start_requires_prior_install() {
    let manager = FakeServiceManager::new();
    assert!(manager.start().is_err(), "start without install must fail");
}

#[test]
fn start_is_idempotent_when_already_running() {
    let home = TempDir::new().unwrap();
    let build = fake_build();
    let sock = scheduler_service::socket_path(home.path());
    let manager = FakeServiceManager::new();
    manager
        .install_or_update(&PathBuf::from("/fake/exe"), &build, &sock)
        .unwrap();
    manager.start().unwrap();
    manager.start().unwrap();
    assert!(manager.is_running());
    manager.stop().unwrap();
}

#[test]
fn setup_service_registers_checkout_and_records_desired_build() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let exe = make_fake_exe(src.path());
    let manager = FakeServiceManager::new();

    let build =
        scheduler_service::setup_service(project.path(), home.path(), &exe, &manager).unwrap();

    let canonical = project.path().canonicalize().unwrap();
    let registry = scheduler_service::read_registry(home.path()).unwrap();
    assert!(
        registry.contains_key(&canonical),
        "checkout must be registered after setup"
    );
    assert!(
        manager.is_installed(),
        "service must be installed after setup"
    );
    assert_eq!(
        scheduler_service::read_desired_build(home.path())
            .unwrap()
            .as_ref(),
        Some(&build),
        "desired build must be recorded after setup"
    );
}

#[test]
fn registration_survives_enable_and_disable_transitions() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let exe = make_fake_exe(src.path());
    let manager = FakeServiceManager::new();

    scheduler_service::setup_service(project.path(), home.path(), &exe, &manager).unwrap();
    manager.enable().unwrap();
    manager.disable().unwrap();

    let canonical = project.path().canonicalize().unwrap();
    let registry = scheduler_service::read_registry(home.path()).unwrap();
    assert!(
        registry.contains_key(&canonical),
        "registration must survive enable/disable"
    );
}

#[test]
fn desired_build_updates_when_executable_changes() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let manager = FakeServiceManager::new();

    let exe1 = make_fake_exe(src.path());
    let build1 =
        scheduler_service::setup_service(project.path(), home.path(), &exe1, &manager).unwrap();

    let exe2 = src.path().join("fake-fluent-v2");
    fs::write(&exe2, b"#!/bin/sh\necho fluent-v2\n").unwrap();
    let build2 =
        scheduler_service::setup_service(project.path(), home.path(), &exe2, &manager).unwrap();

    assert_ne!(
        build1.hash, build2.hash,
        "a new exe must produce a new build hash"
    );
    assert_eq!(
        scheduler_service::read_desired_build(home.path())
            .unwrap()
            .as_ref(),
        Some(&build2),
        "desired build must reflect the latest staging"
    );
}

// ─────────────────────────────────────────────────
// Step 3: execution request, idempotent dispatch, events, executor contract
// ─────────────────────────────────────────────────

#[test]
fn attempt_execution_request_round_trips_through_persistence() {
    let project = TempDir::new().unwrap();
    let request =
        fluent::scheduler_service::AttemptExecutionRequest::new("wi-abc", "attempt-1").unwrap();
    let original_id = request.id.clone();

    scheduler_service::persist_request(project.path(), &request).unwrap();
    let loaded = scheduler_service::load_request(project.path(), &original_id).unwrap();

    assert_eq!(
        loaded, request,
        "loaded request must equal the persisted one"
    );
}

#[test]
fn attempt_execution_request_has_nonempty_generated_id() {
    let r = fluent::scheduler_service::AttemptExecutionRequest::new("wi-1", "attempt-1").unwrap();
    assert!(!r.id.is_empty(), "generated request id must be non-empty");
    assert!(!r.created_at.is_empty(), "created_at must be non-empty");
}

#[test]
fn dispatch_submission_is_exact_bound_and_idempotent() {
    let project = TempDir::new().unwrap();
    let request =
        fluent::scheduler_service::AttemptExecutionRequest::new("wi-1", "attempt-1").unwrap();
    scheduler_service::persist_request(project.path(), &request).unwrap();

    let token1 = scheduler_service::submit_dispatch(project.path(), &request).unwrap();
    let token2 = scheduler_service::submit_dispatch(project.path(), &request).unwrap();

    assert_eq!(
        token1, token2,
        "idempotent dispatch must return the same token"
    );
    assert_eq!(token1.work_item_id, "wi-1");
    assert_eq!(token1.attempt_id, "attempt-1");
    assert_eq!(token1.request_id, request.id);
}

#[test]
fn dispatch_requires_persisted_request() {
    let project = TempDir::new().unwrap();
    let request =
        fluent::scheduler_service::AttemptExecutionRequest::new("wi-1", "attempt-1").unwrap();
    let result = scheduler_service::submit_dispatch(project.path(), &request);
    assert!(
        result.is_err(),
        "dispatch must fail if the request is not persisted"
    );
}

#[test]
fn events_are_appended_and_replayed_in_sequence_order() {
    let project = TempDir::new().unwrap();
    let attempt_id = "attempt-1";

    let e2 = make_event(2, attempt_id, "completed");
    let e0 = make_event(0, attempt_id, "started");
    let e1 = make_event(1, attempt_id, "progress");

    scheduler_service::append_event(project.path(), &e2).unwrap();
    scheduler_service::append_event(project.path(), &e0).unwrap();
    scheduler_service::append_event(project.path(), &e1).unwrap();

    let events = scheduler_service::replay_events(project.path(), attempt_id).unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(events[0].seq, 0);
    assert_eq!(events[1].seq, 1);
    assert_eq!(events[2].seq, 2);
    assert_eq!(events[0].kind, "started");
    assert_eq!(events[2].kind, "completed");
}

#[test]
fn events_for_different_attempts_are_stored_independently() {
    let project = TempDir::new().unwrap();

    scheduler_service::append_event(project.path(), &make_event(0, "attempt-1", "started"))
        .unwrap();
    scheduler_service::append_event(project.path(), &make_event(0, "attempt-2", "started"))
        .unwrap();
    scheduler_service::append_event(project.path(), &make_event(1, "attempt-1", "completed"))
        .unwrap();

    let a1 = scheduler_service::replay_events(project.path(), "attempt-1").unwrap();
    let a2 = scheduler_service::replay_events(project.path(), "attempt-2").unwrap();
    assert_eq!(a1.len(), 2, "attempt-1 must have 2 events");
    assert_eq!(a2.len(), 1, "attempt-2 must have 1 event");
}

#[test]
fn replay_returns_empty_for_unknown_attempt() {
    let project = TempDir::new().unwrap();
    let events = scheduler_service::replay_events(project.path(), "attempt-999").unwrap();
    assert!(
        events.is_empty(),
        "replay of unknown attempt must return empty"
    );
}

#[test]
fn executor_validates_well_formed_request_without_launching() {
    let project = TempDir::new().unwrap();
    let request =
        fluent::scheduler_service::AttemptExecutionRequest::new("wi-exec", "attempt-1").unwrap();
    scheduler_service::persist_request(project.path(), &request).unwrap();

    let validated =
        scheduler_service::validate_executor_request(project.path(), &request.id).unwrap();
    assert_eq!(validated.request.work_item_id, "wi-exec");
    assert_eq!(validated.request.attempt_id, "attempt-1");
}

#[test]
fn executor_rejects_missing_request() {
    let project = TempDir::new().unwrap();
    let result = scheduler_service::validate_executor_request(project.path(), "nonexistent-id");
    assert!(result.is_err(), "validation must fail for missing request");
}

#[test]
fn executor_rejects_request_with_empty_work_item_id() {
    let project = TempDir::new().unwrap();
    let path = project
        .path()
        .join(".fluent/work/scheduler/requests/bad-req.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        r#"{"id":"bad-req","work_item_id":"","attempt_id":"attempt-1","created_at":"2024-01-01T00:00:00Z"}"#,
    )
    .unwrap();
    let result = scheduler_service::validate_executor_request(project.path(), "bad-req");
    assert!(result.is_err(), "validation must reject empty work_item_id");
}

#[test]
fn executor_rejects_request_with_empty_attempt_id() {
    let project = TempDir::new().unwrap();
    let path = project
        .path()
        .join(".fluent/work/scheduler/requests/bad-req2.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        r#"{"id":"bad-req2","work_item_id":"wi-1","attempt_id":"","created_at":"2024-01-01T00:00:00Z"}"#,
    )
    .unwrap();
    let result = scheduler_service::validate_executor_request(project.path(), "bad-req2");
    assert!(result.is_err(), "validation must reject empty attempt_id");
}

// ─────────────────────────────────────────────────
// Step 4: failure isolation and resilience
// ─────────────────────────────────────────────────

#[test]
fn interrupted_staging_leaves_managed_executable_intact() {
    let home = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();

    let exe1 = make_fake_exe(src.path());
    scheduler_service::stage_executable(&exe1, home.path()).unwrap();

    let bad = src.path().join("does-not-exist");
    let result = scheduler_service::stage_executable(&bad, home.path());
    assert!(result.is_err(), "staging a missing file must fail");

    let managed = scheduler_service::managed_executable_path(home.path());
    assert!(
        managed.exists(),
        "managed executable must still exist after failed staging"
    );
    assert_eq!(
        fs::read(&managed).unwrap(),
        fs::read(&exe1).unwrap(),
        "managed executable must retain original content after failed staging"
    );
}

#[test]
fn malformed_registry_entry_does_not_prevent_healthy_project_registration() {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();

    let reg_path = home.path().join(".config/fluent/scheduler/registry.json");
    fs::create_dir_all(reg_path.parent().unwrap()).unwrap();
    fs::write(
        &reg_path,
        r#"{"checkouts":{"/nonexistent/path":"deadbeef"}}"#,
    )
    .unwrap();

    let identity = scheduler_service::assign_checkout_identity(project.path()).unwrap();
    scheduler_service::register_checkout(home.path(), project.path(), &identity).unwrap();

    let registry = scheduler_service::read_registry(home.path()).unwrap();
    let canonical = project.path().canonicalize().unwrap();
    assert!(
        registry.contains_key(&canonical),
        "healthy project must be registered despite stale entry"
    );
    assert!(
        registry.contains_key(&std::path::PathBuf::from("/nonexistent/path")),
        "stale entry must be preserved in registry"
    );
}

#[test]
fn two_projects_register_and_track_independently() {
    let home = TempDir::new().unwrap();
    let p1 = TempDir::new().unwrap();
    let p2 = TempDir::new().unwrap();

    let id1 = scheduler_service::assign_checkout_identity(p1.path()).unwrap();
    let id2 = scheduler_service::assign_checkout_identity(p2.path()).unwrap();
    assert_ne!(id1, id2, "two projects must get distinct identities");

    scheduler_service::register_checkout(home.path(), p1.path(), &id1).unwrap();
    scheduler_service::register_checkout(home.path(), p2.path(), &id2).unwrap();

    let registry = scheduler_service::read_registry(home.path()).unwrap();
    let c1 = p1.path().canonicalize().unwrap();
    let c2 = p2.path().canonicalize().unwrap();
    assert_eq!(registry.get(&c1).unwrap(), &id1);
    assert_eq!(registry.get(&c2).unwrap(), &id2);
}

#[test]
fn one_project_registration_does_not_affect_other_projects_entry() {
    let home = TempDir::new().unwrap();
    let p1 = TempDir::new().unwrap();
    let p2 = TempDir::new().unwrap();

    let id1 = scheduler_service::assign_checkout_identity(p1.path()).unwrap();
    let id2 = scheduler_service::assign_checkout_identity(p2.path()).unwrap();
    scheduler_service::register_checkout(home.path(), p1.path(), &id1).unwrap();
    scheduler_service::register_checkout(home.path(), p2.path(), &id2).unwrap();

    let id1b = fluent::scheduler_service::CheckoutIdentity("updated".to_string());
    scheduler_service::register_checkout(home.path(), p1.path(), &id1b).unwrap();

    let registry = scheduler_service::read_registry(home.path()).unwrap();
    let c2 = p2.path().canonicalize().unwrap();
    assert_eq!(
        registry.get(&c2).unwrap(),
        &id2,
        "p2 identity must be unchanged after p1 update"
    );
}

#[test]
fn duplicate_start_via_fake_manager_is_idempotent() {
    let home = TempDir::new().unwrap();
    let build = fake_build();
    let sock = scheduler_service::socket_path(home.path());
    let manager = FakeServiceManager::new();
    manager
        .install_or_update(&PathBuf::from("/fake/exe"), &build, &sock)
        .unwrap();

    manager.start().unwrap();
    assert!(manager.is_running());
    manager.start().unwrap();
    assert!(
        manager.is_running(),
        "duplicate start must leave service running"
    );

    manager.stop().unwrap();
    assert!(!manager.is_running());
}

#[test]
fn drain_stops_service_while_disable_only_marks_not_autostart() {
    let home = TempDir::new().unwrap();
    let build = fake_build();
    let sock = scheduler_service::socket_path(home.path());
    let manager = FakeServiceManager::new();
    manager
        .install_or_update(&PathBuf::from("/fake/exe"), &build, &sock)
        .unwrap();
    manager.enable().unwrap();
    manager.start().unwrap();

    manager.disable().unwrap();
    assert!(!manager.is_enabled());
    assert!(manager.is_running(), "disable must leave service running");

    manager.drain().unwrap();
    assert!(!manager.is_running(), "drain must stop the running service");
}

#[test]
fn observed_build_is_recorded_independently_of_desired_build() {
    let home = TempDir::new().unwrap();

    let build_a = BuildIdentity {
        version: "1.0.0".to_string(),
        hash: "aaa".to_string(),
    };
    let build_b = BuildIdentity {
        version: "1.1.0".to_string(),
        hash: "bbb".to_string(),
    };

    scheduler_service::record_desired_build(home.path(), &build_b).unwrap();
    scheduler_service::record_observed_build(home.path(), &build_a).unwrap();

    assert_eq!(
        scheduler_service::read_desired_build(home.path())
            .unwrap()
            .as_ref(),
        Some(&build_b)
    );
    assert_eq!(
        scheduler_service::read_observed_build(home.path())
            .unwrap()
            .as_ref(),
        Some(&build_a)
    );
}

// ─────────────────────────────────────────────────
// Step 1 (contract closure): concurrent registry mutations
// ─────────────────────────────────────────────────

// Sentinel test invoked as a child process by
// concurrent_registry_mutations_preserve_every_successful_change. When
// FLUENT_REGISTRY_CHILD is absent (normal nextest runs), returns immediately
// as a harmless no-op.
#[test]
fn __registry_child_worker() {
    let action = match std::env::var("FLUENT_REGISTRY_CHILD") {
        Ok(a) => a,
        Err(_) => return,
    };
    let home = std::env::var("FLUENT_REGISTRY_CHILD_HOME").expect("FLUENT_REGISTRY_CHILD_HOME");
    let project_str =
        std::env::var("FLUENT_REGISTRY_CHILD_PROJECT").expect("FLUENT_REGISTRY_CHILD_PROJECT");
    let project = std::path::Path::new(&project_str);
    match action.as_str() {
        "register" => {
            let id = std::env::var("FLUENT_REGISTRY_CHILD_IDENTITY")
                .expect("FLUENT_REGISTRY_CHILD_IDENTITY");
            let identity = fluent::scheduler_service::CheckoutIdentity(id);
            fluent::scheduler_service::register_checkout(
                std::path::Path::new(&home),
                project,
                &identity,
            )
            .expect("register_checkout in child process");
        }
        "unregister" => {
            fluent::scheduler_service::unregister_checkout(
                std::path::Path::new(&home),
                project,
            )
            .expect("unregister_checkout in child process");
        }
        other => panic!("unknown FLUENT_REGISTRY_CHILD action: {other}"),
    }
}

fn spawn_registry_child(
    action: &str,
    home: &std::path::Path,
    project: &std::path::Path,
    identity: Option<&str>,
) -> std::process::Child {
    let exe = std::env::current_exe().expect("current_exe");
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("__registry_child_worker")
        .env("FLUENT_REGISTRY_CHILD", action)
        .env("FLUENT_REGISTRY_CHILD_HOME", home)
        .env("FLUENT_REGISTRY_CHILD_PROJECT", project)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if let Some(id) = identity {
        cmd.env("FLUENT_REGISTRY_CHILD_IDENTITY", id);
    }
    cmd.spawn().expect("spawn registry child process")
}

#[test]
fn concurrent_registry_mutations_preserve_every_successful_change() {
    const N: usize = 8;
    let home = TempDir::new().unwrap();
    let projects: Vec<TempDir> = (0..N).map(|_| TempDir::new().unwrap()).collect();
    let identities: Vec<String> = (0..N).map(|i| format!("ident{i:08x}")).collect();

    // Spawn N child processes concurrently; each registers a distinct checkout.
    let mut children: Vec<_> = projects
        .iter()
        .zip(identities.iter())
        .map(|(project, identity)| {
            spawn_registry_child("register", home.path(), project.path(), Some(identity))
        })
        .collect();

    for (i, child) in children.iter_mut().enumerate() {
        let status = child.wait().expect("wait for child process");
        assert!(
            status.success(),
            "child process {i} exited with {status:?}"
        );
    }

    // All N checkouts must appear in the registry with correct identities.
    let registry = scheduler_service::read_registry(home.path()).unwrap();
    for (i, (project, identity)) in projects.iter().zip(identities.iter()).enumerate() {
        let canonical = project.path().canonicalize().unwrap();
        let registered = registry.get(&canonical).unwrap_or_else(|| {
            panic!("project {i} is missing from the registry after concurrent registration")
        });
        assert_eq!(
            registered.0, *identity,
            "project {i} has the wrong identity in the registry"
        );
    }
}

// ─────────────────────────────────────────────────
// Step 2 (contract closure): request immutability and dispatch exact binding
// ─────────────────────────────────────────────────

#[test]
fn request_persistence_is_immutable_and_idempotent() {
    let project = TempDir::new().unwrap();
    let request =
        fluent::scheduler_service::AttemptExecutionRequest::new("wi-imm", "attempt-1").unwrap();

    // First persist: must succeed.
    scheduler_service::persist_request(project.path(), &request).unwrap();

    // Second persist with identical content: idempotent success.
    scheduler_service::persist_request(project.path(), &request).unwrap();

    // Original content must be unchanged.
    let loaded = scheduler_service::load_request(project.path(), &request.id).unwrap();
    assert_eq!(loaded, request, "loaded request must equal the original");

    // Persist with same ID but different work_item_id: must be rejected.
    let mut different = request.clone();
    different.work_item_id = "wi-other".to_string();
    let result = scheduler_service::persist_request(project.path(), &different);
    assert!(
        result.is_err(),
        "persist with conflicting content must return an error"
    );

    // Original content must survive the rejected overwrite.
    let after_rejection = scheduler_service::load_request(project.path(), &request.id).unwrap();
    assert_eq!(
        after_rejection, request,
        "original request must be preserved after a rejected overwrite"
    );
}

#[test]
fn dispatch_rejects_request_or_token_identity_mismatch() {
    let project = TempDir::new().unwrap();

    // Persist a valid request.
    let request =
        fluent::scheduler_service::AttemptExecutionRequest::new("wi-bound", "attempt-1").unwrap();
    scheduler_service::persist_request(project.path(), &request).unwrap();

    // Submitting with a wrong work_item_id must be rejected.
    let mut tampered_wi = request.clone();
    tampered_wi.work_item_id = "wi-other".to_string();
    assert!(
        scheduler_service::submit_dispatch(project.path(), &tampered_wi).is_err(),
        "dispatch with wrong work_item_id must fail"
    );

    // Submitting with a wrong attempt_id must be rejected.
    let mut tampered_at = request.clone();
    tampered_at.attempt_id = "attempt-other".to_string();
    assert!(
        scheduler_service::submit_dispatch(project.path(), &tampered_at).is_err(),
        "dispatch with wrong attempt_id must fail"
    );

    // Correct submission: creates a token.
    let token = scheduler_service::submit_dispatch(project.path(), &request).unwrap();
    assert_eq!(token.request_id, request.id);
    assert_eq!(token.work_item_id, "wi-bound");
    assert_eq!(token.attempt_id, "attempt-1");

    // Idempotent re-submission with correct request: returns the same token.
    let token2 = scheduler_service::submit_dispatch(project.path(), &request).unwrap();
    assert_eq!(token, token2, "idempotent dispatch must return the same token");

    // Corrupt the on-disk token to simulate tampering.
    let token_path = project
        .path()
        .join(".fluent/work/scheduler/dispatches")
        .join(format!("{}.json", request.id));
    let corrupted = fluent::scheduler_service::DispatchToken {
        request_id: request.id.clone(),
        work_item_id: "wi-corrupted".to_string(),
        attempt_id: "attempt-1".to_string(),
    };
    fs::write(&token_path, serde_json::to_vec_pretty(&corrupted).unwrap()).unwrap();

    // Submission must now fail because the stored token doesn't match the
    // persisted request.
    assert!(
        scheduler_service::submit_dispatch(project.path(), &request).is_err(),
        "dispatch must fail when the stored token has a mismatched work_item_id"
    );
}

// ─────────────────────────────────────────────────
// Step 3 (contract closure): oversized frame rejection
// ─────────────────────────────────────────────────

#[test]
#[cfg(feature = "test-support")]
fn oversized_frame_is_rejected_before_payload_read() {
    use std::os::unix::net::UnixStream;

    // Create a connected socket pair without binding — avoids the sandbox
    // restriction on Unix socket bind.
    let (mut server, mut client) = UnixStream::pair().expect("UnixStream::pair");

    // Server: read the request frame, then respond with an oversized length
    // header but no payload. The client must bail before reading any payload.
    let server_handle = std::thread::spawn(move || {
        let mut len_buf = [0u8; 4];
        server.read_exact(&mut len_buf).unwrap();
        let len = u32::from_le_bytes(len_buf) as usize;
        let mut req_buf = vec![0u8; len];
        server.read_exact(&mut req_buf).unwrap();
        // Declare MAX_FRAME_SIZE + 1 bytes without writing any payload.
        let oversized: u32 = (fluent::scheduler_service::MAX_FRAME_SIZE as u32) + 1;
        server.write_all(&oversized.to_le_bytes()).unwrap();
    });

    // Client: write a valid request frame, then try to read the oversized response.
    let req = serde_json::to_vec(&serde_json::json!({
        "kind": "health",
        "generation": fluent::scheduler_service::PROTOCOL_GENERATION
    }))
    .unwrap();
    fluent::scheduler_service::frame_write_test_support(&mut client, &req)
        .expect("write valid request frame");
    let result = fluent::scheduler_service::frame_read_test_support(&mut client);
    assert!(
        result.is_err(),
        "frame_read must return an error for an oversized declared length"
    );
    assert_eq!(
        result.unwrap_err().kind(),
        std::io::ErrorKind::InvalidData,
        "error kind must be InvalidData for an oversized inbound frame"
    );

    server_handle.join().expect("server thread panicked");

    // Also verify that frame_write rejects an outbound payload exceeding the cap.
    let (mut a, _b) = UnixStream::pair().expect("second UnixStream::pair");
    let huge = vec![0u8; fluent::scheduler_service::MAX_FRAME_SIZE + 1];
    let write_result = fluent::scheduler_service::frame_write_test_support(&mut a, &huge);
    assert!(
        write_result.is_err(),
        "frame_write must return an error for a payload exceeding MAX_FRAME_SIZE"
    );
    assert_eq!(
        write_result.unwrap_err().kind(),
        std::io::ErrorKind::InvalidInput,
        "error kind must be InvalidInput for an oversized outbound payload"
    );
}
