use super::*;
use crate::work_status::WorkItemStatus;
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

#[test]
#[cfg(feature = "test-support")]
fn snapshot_profile_reports_failed_input_operation_and_path() {
    let root = tempfile::tempdir().unwrap();
    let input = root.path().join(".fluent/work");
    let error = crate::snapshot_profile::input_digest(root.path()).unwrap_err();
    assert!(
        format!("{error:#}").contains(&format!("inspect input {}", input.display())),
        "{error:#}"
    );
    assert!(
        !input.exists(),
        "the failed read must not create input state"
    );
}

#[test]
#[cfg(feature = "test-support")]
fn snapshot_profile_attributes_git_and_resets_between_reads() {
    use crate::snapshot_profile::{capture, mark};
    let root = tempfile::tempdir().unwrap();
    // Keep Git discovery inside the fixture: Tester temp directories can sit
    // below ancestors that the subprocess sandbox cannot inspect.
    crate::git::run(
        root.path(),
        &["init", "-b", "main"],
        "initialize profile fixture",
    )
    .unwrap();
    let (_, first) = capture(|| {
        mark("enumeration");
        crate::git::run_stdout(root.path(), &["--version"], "profile fixture").unwrap();
        mark("workers");
    });
    assert_eq!(first.git.len(), 1);
    assert_eq!(first.git[0].stage, "enumeration");
    assert!(first.git[0].args.iter().any(|arg| arg == "--version"));
    assert_eq!(first.git[0].cwd, root.path());
    assert!(first.git[0].elapsed_ms > 0.0);
    assert!(first.stages_ms.contains_key("workers"));
    let (_, second) = capture(|| mark("empty"));
    assert!(second.git.is_empty());
    assert!(!second.stages_ms.contains_key("enumeration"));
    let (_, stdin) = capture(|| {
        mark("immutable-object");
        crate::git::run_stdout_with_stdin_and_env(
            root.path(),
            &["hash-object", "--stdin"],
            b"fixture",
            ("GIT_CONFIG_SYSTEM", &root.path().join("no-system-config")),
            "profile stdin",
        )
        .unwrap();
        crate::git::run_with_stdin(
            root.path(),
            &["hash-object", "--stdin"],
            b"fixture",
            "profile stdin",
        )
        .unwrap();
    });
    assert_eq!(stdin.git.len(), 2);
    assert!(
        stdin
            .git
            .iter()
            .all(|call| call.stage == "immutable-object")
    );
    let _ = std::panic::catch_unwind(|| capture(|| panic!("profile fixture")));
    let (_, after_panic) = capture(|| mark("after-panic"));
    assert!(after_panic.git.is_empty());
}

#[test]
fn snapshot_projection_preserves_results_across_refreshes() {
    use crate::work_model::{
        AttemptStatus, MergeCandidateMergeStatus, MergeReviewState, TaskOutput, TaskStatus,
        WorkItem, WorkModelStore,
    };
    fn tree(root: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
        fn visit(root: &Path, path: &Path, out: &mut std::collections::BTreeMap<PathBuf, Vec<u8>>) {
            let metadata = path.symlink_metadata().unwrap();
            let contents = if metadata.is_dir() {
                for entry in std::fs::read_dir(path).unwrap() {
                    visit(root, &entry.unwrap().path(), out);
                }
                Vec::new()
            } else if metadata.is_symlink() {
                std::fs::read_link(path)
                    .unwrap()
                    .as_os_str()
                    .as_encoded_bytes()
                    .to_vec()
            } else {
                std::fs::read(path).unwrap()
            };
            out.insert(path.strip_prefix(root).unwrap().into(), contents);
        }
        let mut out = Default::default();
        visit(root, root, &mut out);
        out
    }
    let root = tempfile::tempdir().unwrap();
    let store = WorkModelStore::new(root.path());
    for id in ["running", "queued", "blocked", "ready", "landed", "unknown"] {
        let mut item = WorkItem::planned(id, id);
        item.add_initial_attempt("attempt-1").unwrap();
        match id {
            "running" => {
                item.attempts[0].status = AttemptStatus::Executing;
                item.attempts[0].tasks[0].status = TaskStatus::Executing;
            }
            "blocked" => {
                item.attempts[0].status = AttemptStatus::NeedsUser;
                item.attempts[0].tasks[0].status = TaskStatus::NeedsUser;
            }
            "landed" => {
                let attempt = &mut item.attempts[0];
                attempt.status = AttemptStatus::Complete;
                attempt.review_state = Some(crate::work_model::AttemptReviewState::Passed);
                attempt.tasks[0].status = TaskStatus::Complete;
                attempt.tasks[0].output = Some(TaskOutput {
                    workspace_id: "candidate".into(),
                    workspace_path: ".".into(),
                    source_branch: "main".into(),
                    base_commit: None,
                    commit: "abc123".into(),
                    no_change: None,
                    learner_canonicalization: None,
                });
                item.create_or_get_merge_candidate("attempt-1").unwrap();
                let candidate = &mut item.merge_candidates[0];
                candidate.merge_review_state = MergeReviewState::Passed;
                candidate.merge_state.status = MergeCandidateMergeStatus::Merged;
                candidate.merge_state.merged_commit = Some("abc123".into());
            }
            _ => {}
        }
        store.create_work_item(&item).unwrap();
    }
    crate::queue::add(root.path(), "queued", None).unwrap();
    std::fs::write(
        root.path().join(".fluent/work/queue/unknown.json"),
        "broken ledger",
    )
    .unwrap();
    std::fs::write(
        root.path().join(".fluent/work/items/malformed.json"),
        "broken item",
    )
    .unwrap();
    let evidence = root
        .path()
        .join(".fluent/work/artifacts/retained-transcript.txt");
    std::fs::create_dir_all(evidence.parent().unwrap()).unwrap();
    std::fs::write(&evidence, "retained evidence").unwrap();
    let running = store.read_work_item_snapshot("running").unwrap();
    let lock =
        crate::lease::task_lock_path(root.path(), "running", &running.attempts[0].tasks[0].id);
    let _running_lease = crate::lease::acquire(&lock).unwrap();
    let canonical = root.path().canonicalize().unwrap();
    let registry = canonical.join(
        ".fluent/work/terminal-sessions/running/attempt-1/attempt-1-write-1/terminal-session.json",
    );
    std::fs::create_dir_all(registry.parent().unwrap()).unwrap();
    let descriptor = crate::terminal_sessions::TerminalDescriptor {
        schema_version: 1,
        source_id: canonical.to_string_lossy().into_owned(),
        project_identity: canonical.to_string_lossy().into_owned(),
        work_item_id: "running".into(),
        attempt_id: "attempt-1".into(),
        task_id: running.attempts[0].tasks[0].id.clone(),
        execution_generation: "retained-generation".into(),
        session_id: "retained-session".into(),
        role: "author".into(),
        provider: "Codex".into(),
        lifecycle: crate::terminal_sessions::TerminalLifecycle::Ended,
        can_observe: false,
        can_control: false,
        unavailable_reason: None,
    };
    std::fs::write(&registry, serde_json::to_vec(&serde_json::json!({
        "descriptor": descriptor,
        "registry_path": registry,
        "execution_record": canonical.join(".fluent/work/artifacts/running/attempt-1/attempt-1-write-1/execution.json"),
        "socket": null,
    })).unwrap()).unwrap();
    let before = tree(root.path());
    let first = project::SourceSnapshot::load(root.path()).unwrap();
    #[cfg(feature = "test-support")]
    let (second, profile) =
        crate::snapshot_profile::capture(|| project::SourceSnapshot::load(root.path()).unwrap());
    #[cfg(feature = "test-support")]
    {
        for stage in [
            "work-items",
            "terminals",
            "queue",
            "status-verification-recovery",
            "display-attempt",
            "workers-transitions",
        ] {
            assert!(profile.stages_ms.contains_key(stage), "missing {stage}");
        }
        assert!(profile.operations["read_ledger"] >= 6);
        assert!(profile.operations["display_attempt"] >= 6);
    }
    #[cfg(not(feature = "test-support"))]
    let second = project::SourceSnapshot::load(root.path()).unwrap();
    assert_eq!(first, second);
    let worker = &first
        .work
        .iter()
        .find(|w| w.status.id == "running")
        .unwrap()
        .workers[0];
    assert_eq!(
        worker.key.execution_generation.as_deref(),
        Some("retained-generation")
    );
    let terminal = worker.terminal.as_ref().unwrap();
    assert!(!terminal.can_observe && !terminal.can_control);
    assert!(worker.unavailable_reason.contains("invocation has ended"));
    assert_eq!(tree(root.path()), before);
    assert!(first.diagnostics.iter().any(|d| d.contains("malformed")));
    for (id, category) in [
        ("running", project::Category::Running),
        ("queued", project::Category::Queued),
        ("blocked", project::Category::NeedsYou),
        ("ready", project::Category::Ready),
        ("landed", project::Category::History),
        ("unknown", project::Category::Unknown),
    ] {
        let work = first.work.iter().find(|w| w.status.id == id).unwrap();
        assert_eq!(work.category, category, "{id}");
        let item = store.read_work_item_snapshot(id).unwrap();
        assert_eq!(
            work.status,
            crate::work_status::summarize_work_item(&item, Some(root.path()))
        );
    }
    let mut changed = store.read_work_item_snapshot("ready").unwrap();
    changed.title = "Updated after the prior snapshot".into();
    store.write_work_item(&changed).unwrap();
    std::fs::remove_file(root.path().join(".fluent/work/queue/unknown.json")).unwrap();
    std::fs::remove_file(&registry).unwrap();
    let changed_bytes = tree(root.path());
    let third = project::SourceSnapshot::load(root.path()).unwrap();
    assert!(
        third
            .work
            .iter()
            .find(|w| w.status.id == "running")
            .unwrap()
            .workers[0]
            .terminal
            .is_none()
    );
    assert_eq!(tree(root.path()), changed_bytes);
    assert_eq!(
        third
            .work
            .iter()
            .find(|w| w.status.id == "ready")
            .unwrap()
            .status
            .title,
        changed.title
    );
    assert_eq!(
        third
            .work
            .iter()
            .find(|w| w.status.id == "unknown")
            .unwrap()
            .category,
        project::Category::Ready
    );
    let items = root.path().join(".fluent/work/items");
    std::fs::rename(&items, root.path().join("saved-items")).unwrap();
    std::fs::write(&items, "collection obstruction").unwrap();
    let failed_bytes = tree(root.path());
    assert!(project::SourceSnapshot::load(root.path()).is_err());
    assert_eq!(tree(root.path()), failed_bytes);
}

#[test]
fn dashboard_snapshot_is_nonmutating() {
    fn tree(root: &Path) -> std::collections::BTreeMap<std::path::PathBuf, Option<Vec<u8>>> {
        fn visit(
            root: &Path,
            path: &Path,
            out: &mut std::collections::BTreeMap<std::path::PathBuf, Option<Vec<u8>>>,
        ) {
            out.insert(
                path.strip_prefix(root).unwrap().to_path_buf(),
                if path.is_dir() {
                    None
                } else {
                    Some(std::fs::read(path).unwrap())
                },
            );
            if path.is_dir() {
                for entry in std::fs::read_dir(path).unwrap() {
                    visit(root, &entry.unwrap().path(), out);
                }
            }
        }
        let mut out = Default::default();
        visit(root, &root.join(".fluent/work"), &mut out);
        out
    }
    let root = tempfile::tempdir().unwrap();
    let store = crate::work_model::WorkModelStore::new(root.path());
    let mut item = crate::work_model::WorkItem::planned("visible", "Published");
    item.add_initial_attempt("attempt-1").unwrap();
    item.attempts[0].status = crate::work_model::AttemptStatus::Executing;
    item.attempts[0].tasks[0].status = crate::work_model::TaskStatus::Executing;
    let area = root.path().join(
        &item.attempts[0].tasks[0]
            .artifact_area
            .as_ref()
            .unwrap()
            .path,
    );
    std::fs::create_dir_all(&area).unwrap();
    std::fs::write(
        area.join("execution.json"),
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 1, "state": "running", "owner_pid": std::process::id(),
            "leader_pid": std::process::id(), "process_group_id": std::process::id(),
            "started_at": "2026-09-06T00:00:00Z", "heartbeat_at": "2026-09-06T00:00:00Z"
        }))
        .unwrap(),
    )
    .unwrap();
    store
        .create_work_item_with_transition(
            &item,
            &crate::work_model::DurableTransitionMetadata {
                origin: crate::work_model::DurableTransitionOrigin::Fluent,
                reason: None,
                evidence_ref: None,
            },
        )
        .unwrap();
    std::fs::write(
        root.path()
            .join(".fluent/work/transactions/unpublished.json"),
        "unfinished journal",
    )
    .unwrap();
    std::fs::remove_dir_all(root.path().join(".fluent/work/locks")).unwrap();
    let before = tree(root.path());
    let snapshot = load_snapshot(root.path()).unwrap();
    assert_eq!(tree(root.path()), before);
    assert_eq!(snapshot.rows[0].status.title, "Published");
    assert!(snapshot.rows[0].latest_transition.is_some());
    let mut app = loading_project_app();
    let mut local = LocalRefresh::new(root.path(), &[], root.path());
    local.request(&mut app, false);
    finish_local_refresh(&mut local, &mut app);
    assert_eq!(tree(root.path()), before);
    assert_eq!(app.snapshot.sources[0].work[0].status.title, "Published");
    assert!(
        app.source_reads
            .values()
            .all(|health| health.last_success.is_some())
    );
    assert!(
        snapshot
            .errors
            .iter()
            .any(|d| d.contains("host-owned recovery is pending"))
    );
}

#[test]
#[cfg(feature = "test-support")]
#[serial_test::serial]
fn dashboard_does_not_replay_pending_root_publication() {
    let root = tempfile::tempdir().unwrap();
    let store = crate::work_model::WorkModelStore::new(root.path());
    let mut item = crate::work_model::WorkItem::planned("dashboard-root", "Published");
    item.add_initial_attempt("attempt-1").unwrap();
    store.create_work_item(&item).unwrap();
    let artifact = root
        .path()
        .join(".fluent/work/artifacts/dashboard-root/history.bin");
    std::fs::create_dir_all(artifact.parent().unwrap()).unwrap();
    std::fs::write(&artifact, b"retained evidence").unwrap();
    let mut target = store.read_work_item("dashboard-root").unwrap();
    target.title = "Unpublished".into();
    target.attempts[0].tasks[0].instructions = Some("new child".into());
    unsafe {
        std::env::set_var(
            "FLUENT_TEST_WORK_MODEL_INTERRUPT",
            "before-root-publication:dashboard-root",
        )
    };
    let result = store.write_work_item(&target);
    unsafe { std::env::remove_var("FLUENT_TEST_WORK_MODEL_INTERRUPT") };
    assert!(result.is_err());
    let journal = root
        .path()
        .join(".fluent/work/transactions/dashboard-root.json");
    let work = root.path().join(".fluent/work/items/dashboard-root.json");
    let child = root
        .path()
        .join(".fluent/work/attempts/dashboard-root/attempt-1.json");
    let before = [&journal, &work, &child, &artifact].map(|path| std::fs::read(path).unwrap());
    let status = crate::work_status::load_work_status(root.path()).unwrap();
    assert!(
        status
            .errors
            .iter()
            .any(|error| error.contains("host-owned recovery is pending"))
    );
    assert!(store.read_work_item_snapshot("dashboard-root").is_err());
    let snapshot = load_snapshot(root.path()).unwrap();
    assert!(
        snapshot
            .errors
            .iter()
            .any(|error| error.contains("host-owned recovery is pending"))
    );
    assert!(snapshot.rows.is_empty());
    for (path, bytes) in [&journal, &work, &child, &artifact].into_iter().zip(before) {
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }
}

fn row(id: &str, action: &str) -> WorkItemStatus {
    WorkItemStatus {
        recovery_blocker: None,
        verification: None,
        evidence_history: None,
        id: id.into(),
        title: format!("{id} title"),
        attempt: "attempt [running]".into(),
        task: "write [running]".into(),
        review: "pending".into(),
        merge_candidate: "candidate".into(),
        merge: "pending".into(),
        action: action.into(),
        next_action: None,
        metrics: Default::default(),
        compatibility_warnings: vec![],
        release: None,
    }
}
fn app(rows: Vec<WorkItemStatus>) -> App {
    App::new(snapshot::DashboardSnapshot::from_status(
        crate::work_status::WorkStatus {
            rows,
            errors: vec![],
        },
    ))
}
fn app_with_errors(rows: Vec<WorkItemStatus>, errors: Vec<String>) -> App {
    App::new(snapshot::DashboardSnapshot::from_status(
        crate::work_status::WorkStatus { rows, errors },
    ))
}
fn text(app: &App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|f| render::draw(f, app)).unwrap();
    buffer_text(terminal.backend().buffer())
}
fn buffer_text(buffer: &Buffer) -> String {
    let area = buffer.area;
    (area.y..area.y + area.height)
        .map(|y| {
            (area.x..area.x + area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn dashboard_opens_on_current_work() {
    let app = app(vec![row("ready", "task-ready"), row("done", "complete")]);
    assert!(text(&app, 100, 24).contains("Ready (1)"));
    assert!(!text(&app, 100, 24).contains("done title"));
}

#[test]
fn dashboard_projects_latest_published_transition_on_every_load() {
    let root = tempfile::tempdir().unwrap();
    let store = crate::work_model::WorkModelStore::new(root.path());
    let metadata = crate::work_model::DurableTransitionMetadata {
        origin: crate::work_model::DurableTransitionOrigin::SlackThread,
        reason: Some("plan approved".into()),
        evidence_ref: Some("slack-slice:dashboard".into()),
    };
    store
        .create_work_item_with_transition(
            &crate::work_model::WorkItem {
                id: "work-dashboard".into(),
                title: "Dashboard transitions".into(),
                authorization: crate::work_model::ExecutionAuthorization::Proposed,
                ..Default::default()
            },
            &metadata,
        )
        .unwrap();

    let first = load_snapshot(root.path()).unwrap();
    let transition = first.rows[0].latest_transition.as_ref().unwrap();
    assert_eq!(transition.work_item_id, "work-dashboard");
    assert_eq!(transition.transition.new_state, "planned");
    assert_eq!(transition.transition.sequence, 1);
    assert_eq!(load_snapshot(root.path()).unwrap(), first);
    assert!(!root.path().join(".fluent/work/dashboard").exists());
    assert!(!root.path().join(".fluent/slack").exists());

    store
        .mutate_work_item_with_transition("work-dashboard", &metadata, |item| {
            item.authorization = crate::work_model::ExecutionAuthorization::execution_ready(
                crate::work_model::ExecutionAuthority::Human,
            );
            Ok(())
        })
        .unwrap();
    let advanced = load_snapshot(root.path()).unwrap();
    let transition = advanced.rows[0].latest_transition.as_ref().unwrap();
    assert_eq!(transition.transition.sequence, 2);
    assert_eq!(transition.transition.new_state, "authorized");

    let snapshot = load_snapshot(root.path()).unwrap();
    let row = snapshot
        .rows
        .iter()
        .find(|row| row.status.id == "work-dashboard")
        .unwrap();
    assert!(
        layout::detail_values(row)
            .iter()
            .any(|line| line.contains("Latest transition: work-item work-dashboard"))
    );
}
#[test]
fn current_work_is_grouped_with_counts() {
    let app = app(vec![
        row("need", "needs-user"),
        row("run", "executing"),
        row("ready", "planned"),
    ]);
    let rendered = text(&app, 100, 24);
    assert!(rendered.contains("Needs you (1)"));
    assert!(rendered.contains("Running (1)"));
    assert!(rendered.contains("Ready (1)"));
    assert!(
        rendered.find("Needs you (1)") < rendered.find("Running (1)")
            && rendered.find("Running (1)") < rendered.find("Ready (1)")
    );
}
#[test]
fn empty_project_has_no_selection() {
    let app = app(vec![]);
    assert!(app.selected_id().is_none());
    assert!(text(&app, 100, 24).contains("No Work Items found"));
}
#[test]
fn unknown_current_action_remains_visible() {
    let app = app(vec![row("new", "future-action")]);
    assert!(text(&app, 100, 24).contains("future-action"));
}
#[test]
fn initial_selection_uses_first_non_empty_section() {
    let app = app(vec![row("ready", "planned"), row("need", "failed")]);
    assert_eq!(app.selected_id(), Some("need"));
}
#[test]
fn wide_layout_shows_list_and_detail() {
    let app = app(vec![row("work", "task-ready")]);
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal.draw(|f| render::draw(f, &app)).unwrap();
    let buffer = terminal.backend().buffer();
    let rendered = buffer_text(buffer);
    assert!(rendered.contains("Current Work"));
    assert!(rendered.contains("Work Item detail"));
    assert!(rendered.contains("Attempt: attempt [running]"));
    assert!(buffer[(42, 4)].symbol().contains('│'));
}
#[test]
fn detail_shows_canonical_next_action() {
    let mut status = row("work", "task-ready");
    status.next_action = Some("fluent attempt run work".into());
    let app = app(vec![status]);
    assert!(text(&app, 100, 24).contains("fluent attempt run work"));
}
#[test]
fn detail_does_not_invent_next_action() {
    let app = app(vec![row("work", "executing")]);
    assert!(text(&app, 100, 24).contains("No operator action"));
}
#[test]
fn landed_detail_shows_evidence_history() {
    let mut status = row("work", "merged");
    status.evidence_history = Some(crate::work_status::LandedEvidenceHistory {
        merged_commit: "landed-sha".into(),
        tester_results: Some(".fluent/work/artifacts/work/tester-results.json".into()),
        diagnostic: None,
    });
    let snapshot = snapshot::DashboardSnapshot::from_status(crate::work_status::WorkStatus {
        rows: vec![status],
        errors: vec![],
    });
    let detail = layout::detail_values(&snapshot.rows[0]);
    assert!(
        detail
            .iter()
            .any(|line| line == "Landed commit: landed-sha")
    );
    assert!(
        detail
            .iter()
            .any(|line| line.contains("Evidence history: Tester result"))
    );
}
#[test]
fn selection_moves_across_groups_and_scrolls() {
    let mut app = app(vec![row("need", "failed"), row("run", "executing")]);
    app.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
    assert_eq!(app.selected_id(), Some("run"));
    assert_eq!(app.list_scroll, 0);
}
#[test]
fn selection_survives_refresh_reorder_and_filter() {
    let root = tempfile::tempdir().unwrap();
    let mut app = project_app(vec![
        project_source(
            "/local",
            vec![row("first", "executing"), row("selected", "executing")],
        ),
        project_source("ssh:remote", vec![row("remote", "executing")]),
    ]);
    let mut next = project_app(vec![project_source(
        "/local",
        vec![row("selected", "executing"), row("first", "executing")],
    )])
    .snapshot;
    next.sources[0].work[0].status.title = "Updated local".into();
    let mut local = LocalRefresh::new(root.path(), &[], root.path());
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    local.start(&mut app, move || {
        release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        next
    });
    app.handle_key(KeyCode::Down, KeyModifiers::NONE);
    app.handle_key(KeyCode::Tab, KeyModifiers::NONE);
    app.handle_key(KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(app.filter, project::Filter::Running);
    app.select_key(project::RowKey::work("/local", "selected"));
    let selected = app.selected_key.clone();
    // An SSH result arrives after the local read starts.
    let mut remote = project_source("ssh:remote", vec![row("remote-new", "executing")]);
    remote.diagnostics.push("Current remote diagnostic".into());
    app.snapshot.sources[1] = remote.clone();
    release_tx.send(()).unwrap();
    finish_local_refresh(&mut local, &mut app);
    assert_eq!(app.selected_key, selected);
    assert_eq!(app.filter, project::Filter::Running);
    assert_eq!(app.snapshot.sources[1], remote);
    assert_eq!(
        app.snapshot.sources[0].work[0].status.title,
        "Updated local"
    );
    assert!(
        app.snapshot
            .errors
            .iter()
            .any(|error| error.contains("Current remote diagnostic"))
    );
    assert!(text(&app, 140, 24).contains("> Updated local"));
}
#[test]
fn slow_local_refresh_does_not_block_selection() {
    let root = tempfile::tempdir().unwrap();
    let mut app = project_app(vec![project_source(
        "/local",
        vec![row("first", "executing"), row("selected", "executing")],
    )]);
    let mut local = LocalRefresh::new(root.path(), &[], root.path());
    let next = app.snapshot.clone();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    local.start(&mut app, move || {
        started_tx.send(()).unwrap();
        release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        next
    });
    started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    // Poll ticks cannot overlap the gated read or wait for it.
    for _ in 0..3 {
        local.request(&mut app, false);
        assert!(!local.poll(&mut app));
    }
    let mut embedded = None;
    app.take_dirty();
    handle_terminal_key(
        &mut app,
        &mut embedded,
        KeyCode::Down.into(),
        |_| unreachable!(),
    );
    assert_eq!(
        app.selected_key,
        Some(project::RowKey::work("/local", "selected"))
    );
    assert!(app.take_dirty());
    assert!(text(&app, 140, 24).contains("> selected title"));
    handle_terminal_key(
        &mut app,
        &mut embedded,
        KeyCode::Tab.into(),
        |_| unreachable!(),
    );
    assert_eq!(app.filter, project::Filter::NeedsYou);
    assert!(app.take_dirty());
    assert!(text(&app, 140, 24).contains("No Work Items in this view"));
    handle_terminal_key(
        &mut app,
        &mut embedded,
        KeyCode::Tab.into(),
        |_| unreachable!(),
    );
    assert_eq!(app.filter, project::Filter::Running);
    assert!(text(&app, 140, 24).contains("selected title"));
    // The reader cannot complete until selection, counted views, and draws finish.
    assert!(!local.poll(&mut app));
    release_tx.send(()).unwrap();
    finish_local_refresh(&mut local, &mut app);
    assert!(
        local.pending.is_none(),
        "poll ticks must not queue extra reads"
    );
}

#[test]
fn manual_local_refresh_coalesces_while_a_read_is_pending() {
    let root = tempfile::tempdir().unwrap();
    let store = crate::work_model::WorkModelStore::new(root.path());
    let mut item = crate::work_model::WorkItem::planned("work", "Before manual refresh");
    store.create_work_item(&item).unwrap();
    let mut app = project_app(vec![project::SourceSnapshot::load(root.path()).unwrap()]);
    let mut local = LocalRefresh::new(root.path(), &[], root.path());
    let next = app.snapshot.clone();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    local.start(&mut app, move || {
        release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        next
    });
    item.title = "After manual refresh".into();
    store.write_work_item(&item).unwrap();
    for _ in 0..3 {
        assert_eq!(
            app.handle_key(KeyCode::Char('r'), KeyModifiers::NONE),
            Effect::Refresh
        );
        local.request(&mut app, true);
    }
    release_tx.send(()).unwrap();
    finish_local_refresh(&mut local, &mut app);
    assert_eq!(
        app.snapshot.sources[0].work[0].status.title,
        "Before manual refresh"
    );
    finish_local_refresh(&mut local, &mut app);
    assert_eq!(
        app.snapshot.sources[0].work[0].status.title,
        "After manual refresh"
    );
    assert!(
        local.pending.is_none(),
        "manual requests must coalesce into one follow-up"
    );
    assert!(!local.poll(&mut app));
}

#[test]
fn local_reader_panic_keeps_snapshot_and_allows_retry() {
    let root = tempfile::tempdir().unwrap();
    let mut app = project_app(vec![project_source("/local", vec![row("work", "planned")])]);
    let before = app.snapshot.clone();
    let mut local = LocalRefresh::new(root.path(), &[], root.path());
    local.start(&mut app, || panic!("reader failed"));
    finish_local_refresh(&mut local, &mut app);
    assert_eq!(app.snapshot, before);
    assert!(
        app.stale_error
            .as_ref()
            .unwrap()
            .contains("Local snapshot reader stopped")
    );
    local.start(&mut app, move || before);
    finish_local_refresh(&mut local, &mut app);
    assert!(app.stale_error.is_none());
    assert!(!app.local_read.pending);
    assert!(app.local_read.error.is_none());
    assert!(
        app.source_reads
            .values()
            .all(|health| !health.pending && health.error.is_none())
    );
}

fn finish_local_refresh(local: &mut LocalRefresh, app: &mut App) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !local.poll(app) {
        assert!(Instant::now() < deadline, "local refresh did not finish");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn removed_selection_uses_nearest_remaining_row() {
    let mut app = app(vec![
        row("first", "planned"),
        row("selected", "planned"),
        row("last", "planned"),
    ]);
    app.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
    app.refresh(snapshot::DashboardSnapshot::from_status(
        crate::work_status::WorkStatus {
            rows: vec![row("first", "planned"), row("last", "planned")],
            errors: vec![],
        },
    ));
    assert_eq!(app.selected_id(), Some("last"));
}
#[test]
fn narrow_layout_switches_between_list_and_detail() {
    let mut app = app(vec![row("work", "planned")]);
    app.resize(80, 24);
    assert!(text(&app, 80, 24).contains("Work Items"));
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(text(&app, 80, 24).contains("Work Item detail"));
    app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(text(&app, 80, 24).contains("Work Items"));
}
#[test]
fn undersized_terminal_shows_resize_message() {
    let app = app(vec![row("work", "planned")]);
    assert!(text(&app, 59, 14).contains("Resize terminal"));
}
#[test]
fn overflow_detail_remains_navigable() {
    let mut status = row("work", "planned");
    status.compatibility_warnings = (0..30).map(|n| format!("warning {n}")).collect();
    let mut app = app(vec![status]);
    app.resize(80, 15);
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    for _ in 0..40 {
        app.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
    }
    assert!(app.detail_scroll > 0);
    assert!(text(&app, 80, 15).contains("Warning: warning 29"));
}
#[test]
fn refresh_key_requests_immediate_poll() {
    let mut app = app(vec![row("work", "planned")]);
    assert_eq!(
        app.handle_key(KeyCode::Char('r'), KeyModifiers::NONE),
        Effect::Refresh
    );
}
#[test]
fn failed_poll_keeps_snapshot_and_marks_it_stale() {
    let root = tempfile::tempdir().unwrap();
    let store = crate::work_model::WorkModelStore::new(root.path());
    store
        .create_work_item(&crate::work_model::WorkItem::planned(
            "work",
            "Keep this Work visible",
        ))
        .unwrap();
    let mut app = App::new(load_snapshot(root.path()).unwrap());
    let original = app.snapshot.clone();
    let items = root.path().join(".fluent/work/items");
    let saved = root.path().join(".fluent/work/saved-items");
    std::fs::rename(&items, &saved).unwrap();
    std::fs::write(&items, "cannot enumerate a file as a directory").unwrap();

    refresh(root.path(), &mut app);

    assert_eq!(app.snapshot, original);
    let rendered = text(&app, 100, 24);
    assert!(rendered.contains("STALE:"));
    assert!(rendered.contains("Keep this Work visible"));
    assert!(crate::work_status::load_work_status(root.path()).is_err());

    std::fs::remove_file(&items).unwrap();
    std::fs::rename(&saved, &items).unwrap();
    // Item-local failure is a successful partial refresh and clears staleness.
    std::fs::write(items.join("broken.json"), "invalid item").unwrap();
    refresh(root.path(), &mut app);
    assert!(app.stale_error.is_none());
    assert_eq!(app.snapshot.rows, original.rows);
    assert!(
        app.snapshot
            .errors
            .iter()
            .any(|error| error.contains("broken.json"))
    );
}

#[test]
fn help_lists_contextual_controls_and_closes() {
    let mut app = app(vec![row("work", "planned")]);
    app.handle_key(KeyCode::Char('?'), KeyModifiers::NONE);
    assert!(text(&app, 100, 24).contains("j/k or arrows select"));
    app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(!app.help);
}
#[test]
fn dashboard_controls_leave_work_state_unchanged() {
    fn files(root: &Path) -> std::collections::BTreeMap<std::path::PathBuf, Vec<u8>> {
        fn visit(path: &Path, out: &mut std::collections::BTreeMap<std::path::PathBuf, Vec<u8>>) {
            if path.is_dir() {
                for entry in std::fs::read_dir(path).unwrap() {
                    visit(&entry.unwrap().path(), out);
                }
            } else {
                out.insert(path.to_owned(), std::fs::read(path).unwrap());
            }
        }
        let mut out = Default::default();
        visit(root, &mut out);
        out
    }
    let root = tempfile::tempdir().unwrap();
    let store = crate::work_model::WorkModelStore::new(root.path());
    let mut item = crate::work_model::WorkItem::planned("work", "Read only navigation");
    item.add_initial_attempt("attempt-1").unwrap();
    store.create_work_item(&item).unwrap();
    let before = files(root.path());
    let mut app = project_app(vec![project::SourceSnapshot::load(root.path()).unwrap()]);
    for width in [120, 100, 80, 59] {
        app.resize(width, 24);
        for code in [
            KeyCode::Enter,
            KeyCode::Down,
            KeyCode::Right,
            KeyCode::Left,
            KeyCode::Tab,
            KeyCode::BackTab,
            KeyCode::Char('a'),
            KeyCode::Char('a'),
            KeyCode::Char('?'),
            KeyCode::Esc,
            KeyCode::Char('e'),
            KeyCode::Esc,
            KeyCode::Char('r'),
            KeyCode::Char('c'),
        ] {
            app.handle_key(code, KeyModifiers::NONE);
            let _ = text(&app, width, 24);
        }
    }
    assert_eq!(files(root.path()), before);
}
#[test]
fn overflow_rows_remain_navigable() {
    let mut app = app((0..24)
        .map(|n| row(&format!("work-{n}"), "planned"))
        .collect());
    app.resize(80, 15);
    for _ in 0..23 {
        app.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
    }
    assert_eq!(app.selected_id(), Some("work-23"));
    assert!(app.list_scroll > 0);
    assert!(text(&app, 80, 15).contains("work-23"));
}
#[test]
fn list_navigation_moves_selection_when_scrolled() {
    let mut app = app((0..24)
        .map(|n| row(&format!("work-{n}"), "planned"))
        .collect());
    app.resize(80, 15);
    for _ in 0..23 {
        app.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
    }
    assert!(app.list_scroll > 0);
    app.handle_key(KeyCode::Char('k'), KeyModifiers::NONE);
    assert_eq!(app.selected_id(), Some("work-22"));
}
#[test]
fn all_work_adds_terminal_group() {
    let mut app = app(vec![
        row("current", "planned"),
        row("complete", "complete"),
        row("merged", "merged"),
        row("abandoned", "abandoned"),
    ]);
    app.handle_key(KeyCode::Char('a'), KeyModifiers::NONE);
    let rendered = text(&app, 100, 24);
    assert!(rendered.contains("Terminal (3)"));
    for (id, title) in [
        ("complete", "complete title"),
        ("merged", "merged title"),
        ("abandoned", "abandoned title"),
    ] {
        app.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(app.selected_id(), Some(id));
        assert!(text(&app, 100, 24).contains(title), "missing {title}");
    }
}
#[test]
fn current_empty_state_points_to_all_work() {
    assert!(
        text(&app(vec![row("done", "complete")]), 100, 24)
            .contains("No Current Work. Press a for All Work.")
    );
}
#[test]
fn detail_shows_selected_work_state() {
    let mut status = row("work", "planned");
    status.compatibility_warnings = vec!["legacy state".into()];
    status.metrics.input_tokens = 42;
    status.release = Some(Default::default());
    let rendered = text(&app(vec![status]), 100, 24);
    for value in [
        "work title",
        "ID: work",
        "Action: planned",
        "Attempt: attempt [running]",
        "Task: write [running]",
        "Review: pending",
        "Merge Candidate: candidate",
        "Merge: pending",
        "Metrics:",
        "Warning: legacy state",
        "Release:",
    ] {
        assert!(rendered.contains(value), "missing {value}");
    }
}
#[test]
fn long_and_wide_character_content_stays_within_regions() {
    let mut status = row("識別子", "planned");
    status.title = "e\u{301}識別子 with a very long title that must truncate".into();
    let app = app(vec![status]);
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal.draw(|f| render::draw(f, &app)).unwrap();
    let buffer = terminal.backend().buffer();
    let rendered = buffer_text(buffer);
    assert!(rendered.contains("…"));
    assert!(rendered.contains("[planned]"));
    for y in 4..22 {
        assert!(buffer[(42, y)].symbol().contains('│'));
    }
}
#[test]
fn resize_preserves_selection_and_changes_layout() {
    let mut app = app(vec![row("first", "planned"), row("selected", "planned")]);
    app.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
    app.resize(80, 24);
    assert!(text(&app, 80, 24).contains("Work Items"));
    app.resize(100, 24);
    assert_eq!(app.selected_id(), Some("selected"));
    assert!(text(&app, 100, 24).contains("Work Item detail"));
}
#[test]
fn all_key_toggles_current_and_all_work() {
    let mut app = app(vec![row("work", "planned")]);
    app.handle_key(KeyCode::Char('a'), KeyModifiers::NONE);
    assert!(app.all_work());
    app.handle_key(KeyCode::Char('a'), KeyModifiers::NONE);
    assert!(!app.all_work());
}
#[test]
fn successful_poll_replaces_dashboard_snapshot() {
    let mut app = app(vec![row("old", "planned")]);
    app.take_dirty();
    app.refresh(snapshot::DashboardSnapshot::from_status(
        crate::work_status::WorkStatus {
            rows: vec![row("fresh", "planned")],
            errors: vec![],
        },
    ));
    assert!(app.take_dirty());
    assert_eq!(app.selected_id(), Some("fresh"));
}
#[test]
fn successful_poll_clears_stale_state() {
    let mut app = app(vec![row("work", "planned")]);
    app.refresh_failed("read failed".into());
    app.refresh(app.snapshot.clone());
    assert!(app.stale_error.is_none());
}
#[test]
fn work_read_errors_remain_available_with_overflow() {
    let mut app = app_with_errors(
        vec![row("work", "planned")],
        (0..20).map(|n| format!("error {n}")).collect(),
    );
    app.resize(80, 15);
    for _ in 0..22 {
        app.handle_key(KeyCode::PageDown, KeyModifiers::NONE);
    }
    assert_eq!(
        app.list_scroll as usize,
        app.scroll_bounds().list_max_scroll as usize
    );
    assert!(text(&app, 80, 15).contains("error 19"));
}
#[test]
fn idle_dashboard_does_not_request_repaint() {
    let mut app = app(vec![row("work", "planned")]);
    app.take_dirty();
    app.refresh(app.snapshot.clone());
    assert!(!app.take_dirty());
}
#[test]
fn help_bar_tracks_current_view() {
    let mut app = app(vec![row("work", "planned")]);
    assert!(text(&app, 100, 24).contains("j/k select"));
    app.resize(80, 24);
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(text(&app, 80, 24).contains("Esc list  j/k scroll"));
}
#[test]
fn scheduled_poll_waits_for_its_deadline() {
    let now = Instant::now();
    assert!(!poll_due(now, now + DATA_POLL_INTERVAL));
    assert!(poll_due(now + DATA_POLL_INTERVAL, now + DATA_POLL_INTERVAL));
}

#[test]
fn error_view_exposes_overflow_and_returns_to_selection() {
    let mut app = app_with_errors(
        (0..24)
            .map(|n| row(&format!("work-{n}"), "planned"))
            .collect(),
        (0..30)
            .map(|n| format!("unreadable Work file {n:02}"))
            .collect(),
    );
    app.resize(80, 15);
    let selected = app.selected_id().map(str::to_string);
    assert!(!text(&app, 80, 15).contains("unreadable Work file 00"));
    app.handle_key(KeyCode::Char('e'), KeyModifiers::NONE);
    let opened = text(&app, 80, 15);
    assert!(opened.contains("Work Item read errors"));
    assert!(opened.contains("unreadable Work file 00"));
    assert!(opened.contains("Esc return"));
    for _ in 0..10 {
        app.handle_key(KeyCode::PageDown, KeyModifiers::NONE);
    }
    assert!(text(&app, 80, 15).contains("unreadable Work file 29"));
    app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(text(&app, 80, 15).contains("Work Items"));
    assert_eq!(app.selected_id(), selected.as_deref());
}

fn project_source(id: &str, rows: Vec<WorkItemStatus>) -> project::SourceSnapshot {
    project::SourceSnapshot {
        schema_version: 1,
        id: id.into(),
        host: "local".into(),
        path: std::path::PathBuf::from(id),
        project: "same-name".into(),
        work: rows
            .into_iter()
            .map(|status| project::ProjectWork {
                key: project::RowKey::work(id, &status.id),
                category: project::Category::from_published_action(&status.action, false),
                status,
                phase: None,
                attempt_id: Some("attempt-1".into()),
                candidate_commit: Some("commit-7".into()),
                updated_at: None,
                workers: Vec::new(),
            })
            .collect(),
        queued_work_item_ids: Vec::new(),
        diagnostics: Vec::new(),
        stale: None,
    }
}
fn project_app(sources: Vec<project::SourceSnapshot>) -> App {
    App::new(snapshot::DashboardSnapshot {
        rows: Vec::new(),
        errors: Vec::new(),
        sources,
        project_view: true,
    })
}

#[test]
fn embedded_terminal_keeps_project_context_visible() {
    let mut app = project_app(vec![project_source(
        "/project",
        vec![row("agent-work", "executing")],
    )]);
    let mut screen = crate::terminal_view::Screen::new(77, 22).unwrap();
    screen.write("native 界 agent".as_bytes());
    app.terminal = Some(crate::terminal_view::Display {
        frame: screen.snapshot().unwrap(),
        identity: "agent-work / attempt-1 / writer / generation-1".into(),
        state: "Observe".into(),
        focused: false,
        has_control: false,
    });
    let mut terminal = Terminal::new(TestBackend::new(140, 30)).unwrap();
    terminal.draw(|frame| render::draw(frame, &app)).unwrap();
    let buffer = terminal.backend().buffer();
    let rendered = buffer_text(buffer);
    assert!(rendered.contains("same-name"), "{rendered}");
    let content = layout::terminal_content((140, 30), &app.terminal.as_ref().unwrap().identity);
    assert_eq!(buffer[(content.x + 7, content.y)].symbol(), "界");
    assert_eq!(buffer[(content.x + 10, content.y)].symbol(), "a");
    assert_eq!(buffer[(content.x - 1, content.y)].symbol(), " ");
    let divider = layout::project_areas(ratatui::layout::Rect::new(0, 0, 140, 30)).divider;
    assert_eq!(buffer[(divider.x, content.y)].symbol(), "│");
    assert!(rendered.contains("Observe"), "{rendered}");
    assert!(rendered.contains("agent-work"), "{rendered}");
}

const LONG_TERMINAL_IDENTITY: &str = "codex / 20261011-213742-dashboard-agent-terminals-embedded-agent-terminal / attempt-1 / attempt-1-write-22 / 0123456789abcdef0123456789abcdef";

fn terminal_display() -> crate::terminal_view::Display {
    let mut screen = crate::terminal_view::Screen::new(40, 8).unwrap();
    screen.write(b"native prompt");
    crate::terminal_view::Display {
        frame: screen.snapshot().unwrap(),
        identity: LONG_TERMINAL_IDENTITY.into(),
        state: "Observe".into(),
        focused: false,
        has_control: false,
    }
}

#[test]
fn narrow_terminal_focus_actions_reveal_control() {
    for (width, height) in [(60, 15), (80, 24), (100, 24), (140, 30)] {
        for (key, effect) in [('i', Effect::ControlTerminal), (']', Effect::LiteralEscape)] {
            let mut app = project_app(vec![project_source(
                "/project",
                vec![row("work", "executing")],
            )]);
            app.resize(width, height);
            assert_eq!(
                app.handle_key(KeyCode::Char('t'), KeyModifiers::NONE),
                Effect::OpenTerminal
            );
            app.set_terminal(terminal_display());
            app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
            assert_eq!(app.pane, app::Pane::List);
            assert_eq!(
                app.handle_key(KeyCode::Char(key), KeyModifiers::NONE),
                effect
            );
            app.terminal.as_mut().unwrap().state = "Control · Ctrl-] navigate".into();
            app.terminal.as_mut().unwrap().has_control = true;
            let rendered = text(&app, width, height);
            for expected in [
                "Control · Ctrl-] navigate",
                "attempt-1-write-22",
                "0123456789abcdef0123456789abcdef",
                "native prompt",
            ] {
                assert!(
                    rendered.contains(expected),
                    "{width}x{height}, {key}: missing {expected}: {rendered}"
                );
            }
        }
    }
}

#[test]
fn undersized_terminal_rejects_focus_actions() {
    for (width, height) in [(59, 24), (80, 14), (0, 0)] {
        for key in ['i', ']'] {
            let mut app = project_app(vec![project_source(
                "/project",
                vec![row("work", "executing")],
            )]);
            app.set_terminal(terminal_display());
            app.resize(width, height);
            assert_eq!(
                app.handle_key(KeyCode::Char(key), KeyModifiers::NONE),
                Effect::None,
                "{key} must not focus a hidden terminal at {width}x{height}"
            );
        }
    }
}

#[test]
fn hidden_terminal_rejects_native_input() {
    use crossterm::event::{Event, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
    let mut app = project_app(vec![project_source(
        "/project",
        vec![row("work", "executing")],
    )]);
    app.set_terminal(terminal_display());
    app.handle_key(KeyCode::Char('i'), KeyModifiers::NONE);
    for size in [(59, 24), (80, 14), (80, 24)] {
        app.resize(size.0, size.1);
        if size == (80, 24) {
            app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        }
        for event in [
            Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)),
            Event::Paste("hidden paste".into()),
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 2,
                row: 11,
                modifiers: KeyModifiers::NONE,
            }),
        ] {
            assert!(
                super::terminal_input_event(&app, event.clone()).is_none(),
                "hidden input at {size:?}: {event:?}"
            );
        }
    }
}

#[test]
fn wrapped_terminal_header_preserves_viewport_geometry() {
    use crossterm::event::{Event, MouseButton, MouseEvent, MouseEventKind};
    for (width, height) in [(60, 15), (80, 24), (100, 24), (140, 30)] {
        let mut app = project_app(vec![project_source(
            "/project",
            vec![row("work", "executing")],
        )]);
        app.resize(width, height);
        app.set_terminal(terminal_display());
        app.handle_key(KeyCode::Char('i'), KeyModifiers::NONE);
        let area = layout::terminal_content(app.size, LONG_TERMINAL_IDENTITY);
        let size = layout::terminal_size(app.size, LONG_TERMINAL_IDENTITY);
        assert_eq!(size, (area.width, area.height));
        let mut screen = crate::terminal_view::Screen::new(size.0, size.1).unwrap();
        screen.write(format!("native prompt\x1b[{};{}HZ\x1b[2;3H", size.1, size.0).as_bytes());
        app.terminal.as_mut().unwrap().frame = screen.snapshot().unwrap();
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| render::draw(frame, &app)).unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(area.x, area.y)].symbol(), "n");
        assert_eq!(buffer[(area.right() - 1, area.bottom() - 1)].symbol(), "Z");
        let header_start = layout::project_areas(ratatui::layout::Rect::new(0, 0, width, height))
            .detail
            .y
            + 1;
        let header: String = (header_start..area.y)
            .flat_map(|y| (area.x..area.right()).map(move |x| buffer[(x, y)].symbol()))
            .collect();
        let compact = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
        assert_eq!(
            compact(&header),
            compact(LONG_TERMINAL_IDENTITY),
            "{width}x{height}: {header}"
        );
        assert_eq!(
            terminal.get_cursor_position().unwrap(),
            (area.x + 2, area.y + 1).into()
        );
        let mouse = |column, row| {
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column,
                row,
                modifiers: KeyModifiers::NONE,
            })
        };
        assert!(super::terminal_input_event(&app, mouse(area.x, area.y - 1)).is_none());
        assert_eq!(
            super::terminal_input_event(&app, mouse(area.x + 2, area.y + 1)),
            Some(mouse(2, 1))
        );
        assert!(super::terminal_input_event(&app, mouse(area.right(), area.y)).is_none());
    }
}

#[test]
fn terminal_controls_require_an_open_view() {
    let mut app = project_app(vec![project_source(
        "/project",
        vec![row("work", "executing")],
    )]);
    assert_eq!(
        app.handle_key(KeyCode::Char('i'), KeyModifiers::NONE),
        Effect::None
    );
    app.set_terminal(crate::terminal_view::Display {
        frame: crate::terminal_view::Screen::new(40, 8)
            .unwrap()
            .snapshot()
            .unwrap(),
        identity: "selected generation".into(),
        state: "Observe".into(),
        focused: false,
        has_control: false,
    });
    assert_eq!(
        app.handle_key(KeyCode::Char('i'), KeyModifiers::NONE),
        Effect::ControlTerminal
    );
    assert_eq!(
        app.handle_key(KeyCode::Char('o'), KeyModifiers::NONE),
        Effect::ObserveTerminal
    );
    assert_eq!(
        app.handle_key(KeyCode::Char(']'), KeyModifiers::NONE),
        Effect::LiteralEscape
    );
    assert_eq!(
        app.handle_key(KeyCode::Char('f'), KeyModifiers::NONE),
        Effect::FullscreenTerminal
    );
    app.resize(150, 40);
    let mut refreshed = app.snapshot.clone();
    refreshed.errors.push("attachment failed".into());
    refreshed.sources[0].work.clear();
    app.refresh(refreshed);
    assert_eq!(app.selected_key, None);
    assert_eq!(
        app.terminal.as_ref().unwrap().identity,
        "selected generation"
    );
    assert_eq!(app.size, (150, 40));
    app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
    let filter = app.filter;
    app.handle_key(KeyCode::Char('F'), KeyModifiers::SHIFT);
    assert_ne!(app.filter, filter);
}

#[test]
fn project_groups_disambiguate_sources() {
    let root = tempfile::tempdir().unwrap();
    let first = root.path().join("first/same-name");
    let second = root.path().join("second/same-name");
    std::fs::create_dir_all(&first).unwrap();
    std::fs::create_dir_all(&second).unwrap();
    let home = root.path().join("home");
    let registry = home.join(".config/fluent/scheduler/registry.json");
    std::fs::create_dir_all(registry.parent().unwrap()).unwrap();
    std::fs::write(
        &registry,
        serde_json::json!({"checkouts": {second.to_str().unwrap(): "identity"}}).to_string(),
    )
    .unwrap();
    let (paths, errors) = project::discover_paths(&first, &[first.clone(), second.clone()], &home);
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(paths.len(), 2);
    std::fs::write(&registry, "malformed").unwrap();
    let (_, malformed) = project::discover_paths(&first, &[], &home);
    assert!(
        malformed
            .iter()
            .any(|error| error.contains("Scheduler registry"))
    );
    let app = project_app(vec![
        project_source(first.to_str().unwrap(), vec![row("same", "failed")]),
        project_source(second.to_str().unwrap(), vec![row("same", "planned")]),
    ]);
    assert_eq!(
        app.selected_key.as_ref().unwrap().source_id,
        first.to_str().unwrap()
    );
    let rendered = text(&app, 100, 24);
    assert!(rendered.contains("first/same-name") && rendered.contains("second/same-name"));
}

#[test]
fn project_view_prioritizes_initial_attention() {
    let app = project_app(vec![
        project_source(
            "/one",
            vec![row("planned", "planned"), row("running", "executing")],
        ),
        project_source("/two", vec![row("needs", "needs-user")]),
    ]);
    assert_eq!(app.selected_key.as_ref().unwrap().work_item_id, "needs");
    let root = tempfile::tempdir().unwrap();
    let mut loading = loading_project_app();
    let mut local = LocalRefresh::new(root.path(), &[], root.path());
    local.start(&mut loading, move || app.snapshot);
    finish_local_refresh(&mut local, &mut loading);
    assert_eq!(loading.selected_id(), Some("needs"));
    let fallback = project_app(vec![project_source(
        "/one",
        vec![row("planned", "planned")],
    )]);
    assert_eq!(fallback.selected_id(), Some("planned"));
}

#[test]
fn work_filters_count_items_from_authoritative_state() {
    use project::Category as C;
    let cases = [
        ("needs-user", false, C::NeedsYou),
        ("executing", false, C::Running),
        ("task-ready", true, C::Queued),
        ("task-ready", false, C::Ready),
        ("learner-not-ready", false, C::Ready),
        ("merge-ready", false, C::ReadyToLand),
        ("planned", false, C::Planned),
        ("merged", false, C::History),
        ("future", false, C::Unknown),
    ];
    for (action, queued, expected) in cases {
        assert_eq!(C::from_published_action(action, queued), expected);
    }
    let mut app = project_app(vec![project_source(
        "/one",
        vec![row("one", "planned"), row("two", "needs-user")],
    )]);
    app.filter = project::Filter::NeedsYou;
    assert_eq!(app.project_keys().len(), 1);
    assert_eq!(app.project_keys()[0].work_item_id, "two");
    assert!(text(&app, 100, 24).contains("Needs you 1"));
    let root = tempfile::tempdir().unwrap();
    let mut queued = crate::work_model::WorkItem::planned("queued", "Queued work");
    queued.add_initial_attempt("attempt-1").unwrap();
    crate::work_model::WorkModelStore::new(root.path())
        .create_work_item(&queued)
        .unwrap();
    crate::queue::add(root.path(), "queued", None).unwrap();
    let snapshot = project::SourceSnapshot::load(root.path()).unwrap();
    assert_eq!(snapshot.work[0].category, C::Queued);
    assert_eq!(snapshot.queued_work_item_ids, vec!["queued"]);
    std::fs::write(
        root.path().join(".fluent/work/queue/queued.json"),
        "broken ledger",
    )
    .unwrap();
    let uncertain = project::SourceSnapshot::load(root.path()).unwrap();
    assert_eq!(uncertain.work[0].category, C::Unknown);
    assert!(
        uncertain
            .diagnostics
            .iter()
            .any(|line| line.contains("Queue state for queued unavailable"))
    );
    let proposed_root = tempfile::tempdir().unwrap();
    let mut proposed = crate::work_model::WorkItem::planned("proposed", "Awaiting approval");
    proposed.authorization = crate::work_model::ExecutionAuthorization::Proposed;
    crate::work_model::WorkModelStore::new(proposed_root.path())
        .create_work_item(&proposed)
        .unwrap();
    assert_eq!(
        project::SourceSnapshot::load(proposed_root.path())
            .unwrap()
            .work[0]
            .category,
        C::Planned
    );

    fn candidate_with_learning(
        learning: Option<crate::work_model::AttemptLearning>,
    ) -> crate::work_model::WorkItem {
        use crate::work_model::{
            AttemptReviewState, AttemptStatus, TaskOutput, TaskStatus, WorkspaceAccess,
            WorkspaceRef,
        };
        let mut item = crate::work_model::WorkItem::planned("candidate", "Candidate");
        item.add_initial_attempt("attempt-1").unwrap();
        let attempt = &mut item.attempts[0];
        let task = &mut attempt.tasks[0];
        task.status = TaskStatus::Complete;
        task.workspace_access = WorkspaceAccess {
            reads: vec![WorkspaceRef {
                id: "target".into(),
                path: ".".into(),
            }],
            writes: vec![WorkspaceRef {
                id: "candidate".into(),
                path: "../work-9-candidate-attempt-1".into(),
            }],
        };
        task.output = Some(TaskOutput {
            workspace_id: "candidate".into(),
            workspace_path: "../work-9-candidate-attempt-1".into(),
            source_branch: "main".into(),
            base_commit: None,
            commit: "abc123".into(),
            no_change: None,
            learner_canonicalization: None,
        });
        attempt.status = AttemptStatus::Complete;
        attempt.review_state = Some(AttemptReviewState::Passed);
        attempt.learning = learning;
        item.create_or_get_merge_candidate("attempt-1").unwrap();
        item
    }
    let learning_root = tempfile::tempdir().unwrap();
    let store = crate::work_model::WorkModelStore::new(learning_root.path());
    store
        .create_work_item(&candidate_with_learning(None))
        .unwrap();
    let before = project::SourceSnapshot::load(learning_root.path()).unwrap();
    assert_eq!(before.work[0].category, C::Ready);
    let mut before_app = project_app(vec![before]);
    before_app.filter = project::Filter::Ready;
    assert_eq!(before_app.project_keys().len(), 1);
    assert!(text(&before_app, 140, 24).contains("Ready 1"));
    let learning = crate::work_model::AttemptLearning::succeeded(
        1,
        crate::follow_up::ArtifactRef {
            path: "handoff.json".into(),
            digest: "sha256:x".into(),
        },
    );
    let mut updated = store.read_work_item("candidate").unwrap();
    updated.attempts[0].learning = Some(learning);
    store.write_work_item(&updated).unwrap();
    let after = project::SourceSnapshot::load(learning_root.path()).unwrap();
    assert_eq!(after.work[0].category, C::NeedsYou);
    assert!(
        after.work[0]
            .status
            .next_action
            .as_deref()
            .unwrap_or_default()
            .contains("merge-candidate show")
    );
    let mut after_app = project_app(vec![after]);
    after_app.filter = project::Filter::Ready;
    assert_eq!(after_app.project_keys().len(), 0);
    assert!(text(&after_app, 140, 24).contains("Ready 0"));
}

#[test]
fn expanded_work_shows_parallel_worker_roles() {
    let root = tempfile::tempdir().unwrap();
    let mut item = crate::work_model::WorkItem::planned("work", "Parallel review");
    item.add_review_only_attempt(
        "attempt-1",
        &["architecture", "tests"],
        "main",
        "abcdef",
        true,
    )
    .unwrap();
    crate::work_model::WorkModelStore::new(root.path())
        .create_work_item(&item)
        .unwrap();
    let mut source = project::SourceSnapshot::load(root.path()).unwrap();
    assert_eq!(source.work[0].workers.len(), 2);
    assert_eq!(source.work[0].workers[0].role, "architecture");
    assert_eq!(source.work[0].workers[1].role, "tests");
    assert!(source.work[0].workers[0].terminal.is_none());
    source.work[0].workers[0].terminal = Some(crate::terminal_sessions::TerminalDescriptor {
        schema_version: 1,
        source_id: source.id.clone(),
        project_identity: source.id.clone(),
        work_item_id: "work".into(),
        attempt_id: "attempt-1".into(),
        task_id: source.work[0].workers[0].key.task_id.clone().unwrap(),
        execution_generation: "generation-1".into(),
        session_id: "session-1".into(),
        role: "architecture".into(),
        provider: "Codex".into(),
        lifecycle: crate::terminal_sessions::TerminalLifecycle::Running,
        can_observe: true,
        can_control: true,
        unavailable_reason: None,
    });
    let mut app = project_app(vec![source]);
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.project_keys().len(), 1);
    assert!(text(&app, 140, 30).contains("architecture"));
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(text(&app, 140, 30).contains("Worker: architecture"));
    assert!(text(&app, 140, 30).contains("Observe available"));
    app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
    app.handle_key(KeyCode::Down, KeyModifiers::NONE);
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(text(&app, 140, 30).contains("Worker: tests"));
    assert!(text(&app, 140, 30).contains("Agent terminal:"));
}

#[test]
fn details_distinguish_recorded_values_from_unknowns() {
    let mut source = project_source("/one", vec![row("work", "executing")]);
    let work = &mut source.work[0];
    work.workers.push(project::Worker {
        key: project::RowKey {
            source_id: "/one".into(),
            work_item_id: "work".into(),
            attempt_id: Some("attempt-1".into()),
            task_id: Some("write".into()),
            execution_generation: None,
        },
        role: "Writer".into(),
        state: "executing".into(),
        provider: Some("Codex".into()),
        model: None,
        effort: None,
        elapsed_ms: None,
        updated_at: None,
        terminal: None,
        unavailable_reason: "headless execution".into(),
    });
    let mut app = project_app(vec![source]);
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    app.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
    let details =
        layout::project_detail_values(&app.snapshot.sources, app.selected_worker.as_ref().unwrap());
    assert!(details.contains(&"Provider: Codex".into()));
    assert!(details.contains(&"Model: unavailable".into()));
    assert!(details.contains(&"Elapsed: unavailable".into()));
    assert!(
        details
            .iter()
            .any(|value| value.contains("headless execution"))
    );
    let worker = &mut app.snapshot.sources[0].work[0].workers[0];
    worker.model = Some("test-model".into());
    worker.effort = Some("high".into());
    worker.elapsed_ms = Some(12_000);
    worker.key.execution_generation = Some("generation-1".into());
    worker.terminal = Some(crate::terminal_sessions::TerminalDescriptor {
        schema_version: 1,
        source_id: "/one".into(),
        project_identity: "/one".into(),
        work_item_id: "work".into(),
        attempt_id: "attempt-1".into(),
        task_id: "write".into(),
        execution_generation: "generation-1".into(),
        session_id: "session-1".into(),
        role: "Writer".into(),
        provider: "Codex".into(),
        lifecycle: crate::terminal_sessions::TerminalLifecycle::Running,
        can_observe: true,
        can_control: true,
        unavailable_reason: None,
    });
    let selected = worker.key.clone();
    app.snapshot.sources[0].work[0]
        .status
        .metrics
        .provider_wait_ms = 4_000;
    app.select_key(selected);
    let rendered = text(&app, 140, 30);
    assert!(rendered.contains("Observe available"));
    assert!(rendered.contains("Provider: Codex"));
    assert!(rendered.contains("Model: test-model"));
    assert!(rendered.contains("Effort: high"));
    assert!(rendered.contains("Elapsed: 12s"));
    assert!(rendered.contains("Work provider wait: 4s"));
    assert!(rendered.contains("Agent terminal: Observe available"));
    let ready = project_app(vec![project_source(
        "/one",
        vec![row("work", "task-ready")],
    )]);
    assert!(
        layout::project_detail_values(
            &ready.snapshot.sources,
            ready.selected_key.as_ref().unwrap()
        )
        .contains(&"Next action: fluent attempt run work".into())
    );
}

#[test]
#[cfg(feature = "test-support")]
fn loaded_worker_metadata_reaches_rows_and_details() {
    use crate::work_model::{AttemptStatus, TaskStatus};
    let root = tempfile::tempdir().unwrap();
    let mut item = crate::work_model::WorkItem::planned("work", "Recorded review");
    item.add_review_only_attempt(
        "attempt-1",
        &["architecture", "tests"],
        "main",
        "abcdef",
        true,
    )
    .unwrap();
    let attempt = &mut item.attempts[0];
    attempt.status = AttemptStatus::Executing;
    attempt.coder_mapping.review.model = "review-model".into();
    attempt.coder_mapping.review.effort = Some("high".into());
    let available_task = attempt.tasks[0].id.clone();
    attempt.tasks[0].status = TaskStatus::Complete;
    attempt.tasks[0].started_at = Some("2026-10-11T10:00:00Z".into());
    attempt.tasks[0].completed_at = Some("2026-10-11T10:00:12Z".into());
    attempt.tasks[1].status = TaskStatus::Planned;
    crate::work_model::WorkModelStore::new(root.path())
        .create_work_item(&item)
        .unwrap();
    let identity = crate::terminal_sessions::TerminalIdentity::fixture(
        root.path(),
        "work",
        "attempt-1",
        &available_task,
    );
    let mut session =
        crate::terminal_sessions::TerminalSession::start_fixture(identity, "sleep 30").unwrap();
    let source = project::SourceSnapshot::load(root.path());
    session.stop().unwrap();
    let source = source.unwrap();
    assert_eq!(source.work[0].workers[0].state, "complete");
    assert_eq!(source.work[0].workers[1].state, "planned");
    assert!(
        source.work[0].workers[0]
            .terminal
            .as_ref()
            .unwrap()
            .can_observe
    );
    assert!(source.work[0].workers[1].terminal.is_none());
    let mut app = project_app(vec![source]);
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    let rendered = text(&app, 140, 30);
    assert!(rendered.contains("Worker: architecture"));
    assert!(rendered.contains("State: complete"));
    let details = text(&app, 140, 30);
    assert!(details.contains("Provider: fixture"));
    assert!(details.contains("Model: review-model"));
    assert!(details.contains("Effort: high"));
    assert!(details.contains("Elapsed: 12s"));
    assert!(details.contains("Agent terminal: Observe available"));
    app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
    app.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    let unavailable = text(&app, 140, 30);
    assert!(unavailable.contains("Worker: tests"));
    assert!(unavailable.contains("State: planned"));
    assert!(unavailable.contains("Agent terminal:"));
    assert!(unavailable.contains("unavailable"));
}

#[test]
fn selection_preserves_composite_worker_identity() {
    let mut first = project_source("/one", vec![row("same", "executing")]);
    let mut second = project_source("/two", vec![row("same", "executing")]);
    for source in [&mut first, &mut second] {
        let key = project::RowKey {
            source_id: source.id.clone(),
            work_item_id: "same".into(),
            attempt_id: Some("attempt-1".into()),
            task_id: Some("write".into()),
            execution_generation: Some("generation-2".into()),
        };
        source.work[0].workers.push(project::Worker {
            key,
            role: "Writer".into(),
            state: "executing".into(),
            provider: None,
            model: None,
            effort: None,
            elapsed_ms: None,
            updated_at: None,
            terminal: None,
            unavailable_reason: "unavailable".into(),
        });
    }
    let mut app = project_app(vec![first.clone(), second.clone()]);
    app.select_key(second.work[0].key.clone());
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    app.select_key(second.work[0].workers[0].key.clone());
    let root = tempfile::tempdir().unwrap();
    let mut local = LocalRefresh::new(root.path(), &[], root.path());
    local.start(&mut app, move || snapshot::DashboardSnapshot {
        rows: vec![],
        errors: vec![],
        sources: vec![second, first],
        project_view: true,
    });
    finish_local_refresh(&mut local, &mut app);
    assert_eq!(app.selected_key.as_ref().unwrap().source_id, "/two");
    assert_eq!(
        app.selected_worker
            .as_ref()
            .unwrap()
            .execution_generation
            .as_deref(),
        Some("generation-2")
    );
    app.resize(80, 20);
    assert_eq!(app.selected_key.as_ref().unwrap().source_id, "/two");
    app.handle_key(KeyCode::Char('k'), KeyModifiers::NONE);
    assert_eq!(app.selected_key.as_ref().unwrap().task_id, None);
    assert_eq!(
        app.selected_worker.as_ref().unwrap().task_id.as_deref(),
        Some("write")
    );
    app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
    app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
    app.handle_key(KeyCode::Char('f'), KeyModifiers::NONE);
    assert!(app.selected_key.is_none());
    app.handle_key(KeyCode::Char('f'), KeyModifiers::NONE);
    assert_eq!(app.selected_key.as_ref().unwrap().source_id, "/two");
    let worker_key = app.snapshot.sources[0].work[0].workers[0].key.clone();
    app.select_key(worker_key);
    let mut changed = app.snapshot.clone();
    changed.sources[0].work[0].workers[0]
        .key
        .execution_generation = Some("generation-3".into());
    app.refresh(changed);
    assert_eq!(
        app.selected_worker, None,
        "never select a replacement invocation implicitly"
    );
    let mut removed = app.snapshot.clone();
    removed.sources[0].work[0].workers.clear();
    removed.sources[1].work[0].status.title = "Neighbor".into();
    app.refresh(removed);
    assert_eq!(
        app.selected_key,
        Some(project::RowKey::work("/two", "same"))
    );
    assert_eq!(app.selected_worker, None);
}

#[test]
fn failed_source_does_not_hide_healthy_projects() {
    let root = tempfile::tempdir().unwrap();
    let one = root.path().join("one");
    let two = root.path().join("two");
    std::fs::create_dir_all(&one).unwrap();
    std::fs::create_dir_all(&two).unwrap();
    let store = crate::work_model::WorkModelStore::new(&one);
    let mut retained = crate::work_model::WorkItem::planned("one", "Retain me");
    retained.add_initial_attempt("attempt-1").unwrap();
    store.create_work_item(&retained).unwrap();
    let mut healthy = crate::work_model::WorkItem::planned("two", "Healthy");
    healthy.add_initial_attempt("attempt-1").unwrap();
    crate::work_model::WorkModelStore::new(&two)
        .create_work_item(&healthy)
        .unwrap();
    let healthy_store = crate::work_model::WorkModelStore::new(&two);
    let mut newer = healthy_store.read_work_item("two").unwrap();
    newer.title = "Healthy now".into();
    healthy_store.write_work_item(&newer).unwrap();
    let old = project::SourceSnapshot::load(&one).unwrap();
    assert_eq!(old.work.len(), 1, "{:?}", old.diagnostics);
    let items = one.join(".fluent/work/items");
    std::fs::remove_dir_all(&items).unwrap();
    std::fs::write(&items, "broken").unwrap();
    let mut app = project_app(vec![old]);
    let mut local = LocalRefresh::new(&one, &[two], root.path());
    local.request(&mut app, false);
    finish_local_refresh(&mut local, &mut app);
    let refreshed = &app.snapshot.sources;
    assert_eq!(refreshed.len(), 2);
    assert!(refreshed[0].stale.is_some());
    assert_eq!(refreshed[0].work[0].status.title, "Retain me");
    assert!(refreshed[1].stale.is_none());
    let rendered = text(&app, 140, 24);
    assert!(rendered.contains("last good Work"));
    assert_eq!(rendered.matches("! ").count(), 1);
    assert!(rendered.contains("Retain me"));
    assert!(rendered.contains("Healthy now"));
    assert!(
        app.snapshot
            .errors
            .iter()
            .any(|error| error.contains("STALE"))
    );
    // A successful partial read clears staleness and retains item diagnostics.
    std::fs::remove_file(&items).unwrap();
    std::fs::create_dir_all(&items).unwrap();
    store.create_work_item(&retained).unwrap();
    std::fs::write(items.join("malformed.json"), "broken").unwrap();
    local.request(&mut app, true);
    finish_local_refresh(&mut local, &mut app);
    assert!(
        app.snapshot
            .sources
            .iter()
            .all(|source| source.stale.is_none())
    );
    assert!(
        app.snapshot
            .errors
            .iter()
            .any(|error| error.contains("malformed.json"))
    );
    assert!(text(&app, 140, 24).contains("Retain me"));
}

#[test]
fn project_layout_keeps_selected_content_visible() {
    let mut app = project_app(vec![project_source(
        "/one",
        (0..30)
            .map(|n| row(&format!("work-{n}"), "planned"))
            .collect(),
    )]);
    app.snapshot.sources[0].work[29].status.title =
        "work-29 漢字 e\u{301} and a long title that must stay inside its pane".into();
    for (width, height) in [
        (59, 14),
        (60, 14),
        (59, 15),
        (60, 15),
        (99, 18),
        (100, 18),
        (140, 24),
    ] {
        app.resize(width, height);
        for _ in 0..29 {
            app.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        }
        let rendered = text(&app, width, height);
        if width < 60 {
            assert!(rendered.contains("Resize terminal"));
            assert!(rendered.contains("q quit"));
        } else if height < 15 {
            assert!(rendered.contains("Resize terminal"));
            assert!(rendered.contains("q quit"));
        } else {
            assert!(rendered.contains("work-29"));
            assert!(rendered.contains("> work-29"));
            assert!(rendered.contains('漢') && rendered.contains('字'));
            assert!(rendered.contains("e\u{301}"));
            assert!(rendered.contains("q quit"));
            assert!(rendered.contains("? help"));
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| render::draw(frame, &app)).unwrap();
            assert!(!buffer_text(terminal.backend().buffer()).contains('│'));
        }
    }
    let mut compact = project_app(vec![project_source(
        "/one",
        vec![row("長い", "needs-user")],
    )]);
    compact.snapshot.sources[0].work[0].status.title =
        "漢字 e\u{301} and a very long Work title that wraps safely".into();
    compact.resize(60, 15);
    compact.handle_key(KeyCode::Right, KeyModifiers::NONE);
    let rendered = text(&compact, 60, 15);
    assert!(rendered.contains("Work Item overview"));
    compact.handle_key(KeyCode::PageDown, KeyModifiers::NONE);
    assert!(text(&compact, 60, 15).contains("Work Item: 長"));
    compact.resize(100, 18);
    let mut terminal = Terminal::new(TestBackend::new(100, 18)).unwrap();
    terminal
        .draw(|frame| render::draw(frame, &compact))
        .unwrap();
    assert!(!buffer_text(terminal.backend().buffer()).contains('│'));
    assert!(buffer_text(terminal.backend().buffer()).contains("Work Item overview"));
}

#[test]
fn project_sources_observe_without_mutating_work_or_registry() {
    fn files(root: &Path) -> std::collections::BTreeMap<std::path::PathBuf, Vec<u8>> {
        fn visit(
            root: &Path,
            path: &Path,
            out: &mut std::collections::BTreeMap<std::path::PathBuf, Vec<u8>>,
        ) {
            if path.is_dir() {
                for entry in std::fs::read_dir(path).unwrap() {
                    visit(root, &entry.unwrap().path(), out);
                }
            } else {
                out.insert(
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    std::fs::read(path).unwrap(),
                );
            }
        }
        let mut out = std::collections::BTreeMap::new();
        visit(root, root, &mut out);
        out
    }
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let mut item = crate::work_model::WorkItem::planned("work", "Read only");
    item.add_initial_attempt("attempt-1").unwrap();
    crate::work_model::WorkModelStore::new(&project)
        .create_work_item(&item)
        .unwrap();
    let before = files(root.path());
    let (paths, errors) = project::discover_paths(&project, &[], root.path());
    assert!(errors.is_empty());
    let source = project::SourceSnapshot::load(&paths[0]).unwrap();
    assert_eq!(source.work.len(), 1);
    let mut app = project_app(vec![source]);
    let mut local = LocalRefresh::new(&project, &[], root.path());
    local.request(&mut app, true);
    finish_local_refresh(&mut local, &mut app);
    assert_eq!(app.snapshot.sources[0].work.len(), 1);
    assert_eq!(files(root.path()), before);
    assert!(
        !root
            .path()
            .join(".config/fluent/scheduler/registry.json")
            .exists()
    );
}

#[test]
fn dashboard_accepts_repeatable_project_paths() {
    use clap::Parser;
    let cli = crate::cli::Cli::try_parse_from([
        "fluent",
        "dashboard",
        "./current",
        "--project",
        "./one",
        "--project",
        "./two",
    ])
    .unwrap();
    match cli.command.unwrap() {
        crate::cli::Commands::Dashboard { path, projects } => {
            assert_eq!(path.as_deref(), Some("./current"));
            assert_eq!(projects, ["./one", "./two"]);
        }
        _ => panic!("expected dashboard command"),
    }
}

#[test]
fn project_layout_uses_header_rule_and_one_pane_divider() {
    let mut app = project_app(vec![project_source(
        "/factory/main",
        vec![row("work", "executing")],
    )]);
    for (width, height) in [(120, 28), (100, 24)] {
        app.resize(width, height);
        for terminal_open in [false, true] {
            app.terminal = terminal_open.then(terminal_display);
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| render::draw(frame, &app)).unwrap();
            let buffer = terminal.backend().buffer();
            let rendered = buffer_text(buffer);
            let lines = rendered.lines().collect::<Vec<_>>();
            assert!(lines[0].ends_with("Fluent"), "{rendered}");
            assert!(
                lines[0]
                    .contains("Current 1  Needs you 0  Running 1  Ready 0  Waiting 0  History 0"),
                "{rendered}"
            );
            assert!(lines[1].chars().all(|c| c == '─'), "{rendered}");
            for line in &lines[2..height as usize - 2] {
                assert_eq!(
                    line.matches('│').count(),
                    usize::from(terminal_open),
                    "{rendered}"
                );
                assert!(!line.contains(['┌', '┐', '└', '┘', '─']), "{rendered}");
            }
            if !terminal_open {
                continue;
            }
            let divider = lines[2].chars().position(|c| c == '│').unwrap();
            assert!(divider >= width as usize * 3 / 5);
            for y in 2..height - 2 {
                assert_eq!(buffer[(divider as u16, y)].symbol(), "│");
            }
            assert!(lines[2].contains("Work Items") && lines[2].contains("Status"));
        }
    }
}

#[test]
fn project_rows_preserve_exact_status_and_selection_cues() {
    let mut source = project_source(
        "/factory/main",
        vec![
            row("long", "executing"),
            row("land", "merge-ready"),
            row("plan", "planned"),
        ],
    );
    source.stale = Some("offline".into());
    source.work[0].status.title =
        "漢字 e\u{301} a very long title that must not hide the exact state".into();
    source.work[0].phase = Some("Writing".into());
    let app = project_app(vec![source]);
    let mut terminal = Terminal::new(TestBackend::new(120, 28)).unwrap();
    terminal.draw(|frame| render::draw(frame, &app)).unwrap();
    let buffer = terminal.backend().buffer();
    let rendered = buffer_text(buffer);
    let lines = rendered.lines().collect::<Vec<_>>();
    assert!(lines[3].contains("! "), "{rendered}");
    assert!(
        lines[4].contains('漢')
            && lines[4].contains('字')
            && lines[4].contains("e\u{301}")
            && lines[4].contains("Running"),
        "{rendered}"
    );
    assert!(lines[5].contains("Ready to land"), "{rendered}");
    assert!(lines[6].contains("Planned"), "{rendered}");
    assert_eq!(buffer[(0, 4)].symbol(), ">");
    assert_ne!(buffer[(0, 4)].bg, buffer[(0, 5)].bg);
    let status_x = lines[2].find("Status").unwrap() as u16;
    assert_eq!(buffer[(status_x, 4)].symbol(), "R");
    assert_eq!(buffer[(status_x, 5)].symbol(), "R");
    assert!(lines[2].contains("Updated"));
}

#[test]
fn tab_views_preserve_selection_and_exact_categories() {
    use project::{Category as C, Filter};
    let mut source = project_source(
        "/one",
        vec![
            row("attention", "needs-user"),
            row("run", "executing"),
            row("ready", "task-ready"),
            row("land", "merge-ready"),
            row("queue", "task-ready"),
            row("plan", "planned"),
            row("past", "merged"),
            row("future", "future"),
        ],
    );
    source.work[4].category = C::Queued;
    let mut app = project_app(vec![source]);
    let selected = app.selected_key.clone();
    assert_eq!(app.project_keys().len(), 7);
    let cases = [
        (Filter::NeedsYou, vec!["attention"]),
        (Filter::Running, vec!["run"]),
        (Filter::Ready, vec!["ready", "land"]),
        (Filter::Waiting, vec!["queue", "plan"]),
        (Filter::History, vec!["past"]),
        (
            Filter::Current,
            vec![
                "attention",
                "run",
                "ready",
                "land",
                "queue",
                "plan",
                "future",
            ],
        ),
    ];
    for (filter, ids) in cases {
        app.handle_key(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.filter, filter);
        assert_eq!(
            app.project_keys()
                .iter()
                .map(|key| key.work_item_id.as_str())
                .collect::<Vec<_>>(),
            ids
        );
        if filter == Filter::NeedsYou {
            assert_eq!(app.selected_key, selected);
        }
        let rendered = text(&app, 120, 28);
        assert!(
            rendered
                .lines()
                .next()
                .unwrap()
                .contains("Current 7  Needs you 1  Running 1  Ready 2  Waiting 2  History 1")
        );
        if filter == Filter::Ready {
            assert!(rendered.contains("Ready to land"));
        }
        if filter == Filter::Waiting {
            assert!(rendered.contains("Queued") && rendered.contains("Planned"));
        }
    }
    app.handle_key(KeyCode::BackTab, KeyModifiers::SHIFT);
    assert_eq!(app.filter, Filter::History);
    let selected = app.selected_key.clone();
    app.handle_key(KeyCode::Tab, KeyModifiers::NONE);
    assert_ne!(app.selected_key, selected);
    app.handle_key(KeyCode::Tab, KeyModifiers::SHIFT);
    assert_eq!(app.filter, Filter::History);
    app.handle_key(KeyCode::Char('f'), KeyModifiers::NONE);
    assert_eq!(app.filter, Filter::Current);
    app.handle_key(KeyCode::Char('a'), KeyModifiers::NONE);
    assert_eq!(app.filter, Filter::Current);
    app.handle_key(KeyCode::Char('a'), KeyModifiers::NONE);
    assert_eq!(app.filter, Filter::Current);
}

#[test]
fn project_rows_show_recorded_update_times() {
    let root = tempfile::tempdir().unwrap();
    let mut item = crate::work_model::WorkItem::planned("work", "Recorded update");
    item.add_initial_attempt("attempt-1").unwrap();
    item.attempts[0].tasks[0].started_at = Some("2026-10-25T11:22:00Z".into());
    let store = crate::work_model::WorkModelStore::new(root.path());
    store
        .create_work_item_with_transition(
            &item,
            &crate::work_model::DurableTransitionMetadata {
                origin: Default::default(),
                reason: None,
                evidence_ref: None,
            },
        )
        .unwrap();
    let saved = store.read_work_item_snapshot("work").unwrap();
    let updated =
        chrono::DateTime::parse_from_rfc3339(&saved.transitions.last().unwrap().occurred_at)
            .unwrap()
            .with_timezone(&chrono::Utc)
            .format("%m-%d %H:%M")
            .to_string();
    let source = project::SourceSnapshot::load(root.path()).unwrap();
    let mut app = project_app(vec![source]);
    let rendered = text(&app, 120, 28);
    assert!(rendered.contains("Updated UTC"), "{rendered}");
    assert!(rendered.contains(&updated), "{rendered}");
    assert_eq!(rendered.matches(&updated).count(), 1, "{rendered}");
}

#[test]
fn details_put_title_reason_and_action_before_identifiers() {
    let mut status = row("WI-42", "needs-user");
    status.title = "Full selected title".into();
    status.recovery_blocker = Some("Host proof is missing".into());
    status.next_action = Some("fluent work-item show WI-42".into());
    let mut source = project_source("/one", vec![status]);
    source.work[0].workers.push(project::Worker {
        key: project::RowKey {
            source_id: "/one".into(),
            work_item_id: "WI-42".into(),
            attempt_id: Some("attempt-1".into()),
            task_id: Some("write-1".into()),
            execution_generation: Some("generation-1".into()),
        },
        role: "writer".into(),
        state: "executing".into(),
        provider: Some("Codex".into()),
        model: None,
        effort: None,
        elapsed_ms: None,
        updated_at: None,
        terminal: None,
        unavailable_reason: "headless execution".into(),
    });
    let mut app = project_app(vec![source]);
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    let overview = app.detail_values().join("\n");
    assert!(overview.starts_with("Full selected title\nReason: Host proof is missing"));
    assert!(overview.find("Next action").unwrap() < overview.find("Workers (").unwrap());
    assert!(overview.find("Workers (").unwrap() < overview.find("Work Item:").unwrap());
    assert!(overview.contains("fluent work-item show WI-42"));
    assert!(text(&app, 120, 28).contains("Full selected title"));
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    let worker = app.detail_values().join("\n");
    for field in [
        "Worker: writer",
        "State: executing",
        "Task: write-1",
        "Invocation: generation-1",
        "headless execution",
    ] {
        assert!(worker.contains(field), "{worker}");
    }
    assert!(!worker.contains("Next action"));
}

#[test]
fn worker_arrow_opens_details_and_t_opens_terminal() {
    let mut source = project_source("/one", vec![row("work", "executing")]);
    let mut available = worker_row("/one", "writer");
    available.terminal = Some(crate::terminal_sessions::TerminalDescriptor {
        schema_version: 1,
        source_id: "/one".into(),
        project_identity: "/one".into(),
        work_item_id: "work".into(),
        attempt_id: "attempt-1".into(),
        task_id: "writer".into(),
        execution_generation: "generation-writer".into(),
        session_id: "selected-session".into(),
        role: "writer".into(),
        provider: "fixture".into(),
        lifecycle: crate::terminal_sessions::TerminalLifecycle::Running,
        can_observe: true,
        can_control: true,
        unavailable_reason: None,
    });
    source.work[0].workers = vec![available, worker_row("/one", "reviewer")];
    let mut app = project_app(vec![source]);
    assert_eq!(
        app.handle_key(KeyCode::Right, KeyModifiers::NONE),
        Effect::None
    );
    assert_eq!(app.pane, app::Pane::Detail);
    assert!(app.terminal.is_none());
    app.pane = app::Pane::List;
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.project_keys().len(), 1);
    let selected = app.selected_worker.clone();
    assert_eq!(
        app.handle_key(KeyCode::Right, KeyModifiers::NONE),
        Effect::None
    );
    assert_eq!(app.pane, app::Pane::Detail);
    assert!(app.terminal.is_none());
    let details = text(&app, 120, 28);
    assert!(details.contains("Worker: writer"), "{details}");
    assert!(
        details.lines().last().unwrap().contains("t terminal"),
        "{details}"
    );
    assert_eq!(
        app.handle_key(KeyCode::Char('t'), KeyModifiers::NONE),
        Effect::OpenTerminal
    );
    assert_eq!(app.selected_worker, selected);
    app.pane = app::Pane::Detail;
    app.show_details = true;
    app.page = model::ProjectPage::Overview;
    app.handle_key(KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(
        app.handle_key(KeyCode::Right, KeyModifiers::NONE),
        Effect::None
    );
    assert!(text(&app, 120, 28).contains("headless execution"));
    assert!(app.terminal.is_none());
    app.pane = app::Pane::Detail;
    app.page = model::ProjectPage::Overview;
    app.handle_key(KeyCode::Up, KeyModifiers::NONE);
    app.snapshot.sources[0].work[0].workers[0]
        .terminal
        .as_mut()
        .unwrap()
        .can_observe = false;
    assert_eq!(
        app.handle_key(KeyCode::Right, KeyModifiers::NONE),
        Effect::None
    );
    app.pane = app::Pane::Detail;
    app.page = model::ProjectPage::Overview;
    app.handle_key(KeyCode::Up, KeyModifiers::NONE);
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.project_keys().len(), 1);
}

#[test]
#[cfg(feature = "test-support")]
fn terminal_shortcut_observes_without_retargeting_on_selection() {
    with_terminal_client(
        "terminal_shortcut_observes_without_retargeting_on_selection",
        || {
            use crate::terminal_sessions::{TerminalIdentity, TerminalSession};
            use crossterm::event::KeyEvent;
            let root = tempfile::tempdir().unwrap();
            let mut item = crate::work_model::WorkItem::planned("work", "Two workers");
            item.add_review_only_attempt(
                "attempt-1",
                &["architecture", "tests"],
                "main",
                "abc",
                true,
            )
            .unwrap();
            let tasks = item.attempts[0]
                .tasks
                .iter()
                .map(|task| task.id.clone())
                .collect::<Vec<_>>();
            crate::work_model::WorkModelStore::new(root.path())
                .create_work_item(&item)
                .unwrap();
            let mut sessions = tasks
                .iter()
                .map(|task| {
                    TerminalSession::start_fixture(
                        TerminalIdentity::fixture(root.path(), "work", "attempt-1", task),
                        "sleep 30",
                    )
                    .unwrap()
                })
                .collect::<Vec<_>>();
            let source = project::SourceSnapshot::load(root.path()).unwrap();
            let mut app = project_app(vec![source]);
            let mut embedded = None;
            app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
            handle_terminal_key(
                &mut app,
                &mut embedded,
                KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE),
                |_| Ok(()),
            );
            assert!(
                embedded.is_some(),
                "open failed: {:?}; selected: {:?}",
                app.stale_error,
                app.selected_key
            );
            let terminal = embedded.as_mut().unwrap();
            assert_terminal_client_ready(terminal);
            assert_eq!(terminal.descriptor(), sessions[0].descriptor());
            assert!(terminal.state_label().starts_with("Observe"));
            assert!(!terminal.focused());
            assert!(!terminal.has_control());
            app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
            handle_terminal_key(
                &mut app,
                &mut embedded,
                KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
                |_| Ok(()),
            );
            assert_eq!(
                embedded.as_ref().unwrap().descriptor(),
                sessions[0].descriptor()
            );
            assert!(
                app.terminal.is_some(),
                "browsing retains the attached invocation"
            );
            handle_terminal_key(
                &mut app,
                &mut embedded,
                KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE),
                |_| Ok(()),
            );
            let terminal = embedded.as_mut().unwrap();
            assert_terminal_client_ready(terminal);
            assert_eq!(terminal.descriptor(), sessions[1].descriptor());
            assert!(terminal.state_label().starts_with("Observe"));
            assert!(!terminal.has_control());
            drop(embedded);
            for session in &mut sessions {
                session.stop().unwrap();
            }
        },
    );
}

#[cfg(feature = "test-support")]
fn with_terminal_client(test: &str, check: impl FnOnce()) {
    if std::env::var("FLUENT_DASHBOARD_TERMINAL_TEST").as_deref() == Ok(test) {
        check();
        return;
    }
    // Isolate the existing client seam from parallel libtest cases. Runtime
    // admission, PTYs and Control leases stay real; only tmux is substituted.
    let root = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("dashboard::tests::{test}"),
            "--nocapture",
        ])
        .env("FLUENT_DASHBOARD_TERMINAL_TEST", test)
        .env(
            "FLUENT_TEST_TERMINAL_CLIENT",
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ssh-terminal-client.py"),
        )
        .env("FLUENT_TEST_TERMINAL_CLIENT_LOG", root.path().join("input"))
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed;"),
        "{test}: {stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(feature = "test-support")]
fn assert_terminal_client_ready(terminal: &mut crate::terminal_view::EmbeddedTerminal) {
    use std::time::{Duration, Instant};
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        terminal.refresh();
        if terminal.frame().text().contains("SSH_FULLSCREEN_PAINT") {
            return;
        }
        assert!(Instant::now() < deadline, "terminal client did not paint");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn narrow_view_keeps_filter_and_return_keys_distinct() {
    let mut app = project_app(vec![project_source(
        "/one",
        vec![row("work", "needs-user")],
    )]);
    for width in [60, 80, 99] {
        app.resize(width, 24);
        app.pane = app::Pane::List;
        app.filter = project::Filter::Current;
        assert!(!text(&app, width, 24).contains("Work Item: work"));
        app.handle_key(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.filter, project::Filter::NeedsYou);
        assert_eq!(app.pane, app::Pane::List);
        app.handle_key(KeyCode::Right, KeyModifiers::NONE);
        let rendered = text(&app, width, 24);
        assert!(rendered.contains("Work Item: work"), "{rendered}");
        assert!(rendered.contains("Esc Back"), "{rendered}");
        app.handle_key(KeyCode::BackTab, KeyModifiers::SHIFT);
        assert_eq!(app.filter, project::Filter::NeedsYou);
        assert_eq!(app.pane, app::Pane::Detail);
        app.handle_key(KeyCode::Left, KeyModifiers::NONE);
        assert_eq!(app.pane, app::Pane::List);
        assert!(text(&app, width, 24).contains("work title"));
        app.handle_key(KeyCode::Right, KeyModifiers::NONE);
        app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(app.pane, app::Pane::List);
    }
}

#[test]
fn terminal_mode_and_focus_remain_explicit() {
    for (width, height) in [(120, 28), (100, 24), (80, 24), (60, 15)] {
        for state in [
            "Observe · i control",
            "Control · Ctrl-] navigate",
            "Control · navigation",
        ] {
            let mut app = project_app(vec![project_source("/one", vec![row("work", "executing")])]);
            app.resize(width, height);
            app.pane = app::Pane::Detail;
            let mut display = terminal_display();
            display.state = state.into();
            display.has_control = state.starts_with("Control");
            app.set_terminal(display);
            let rendered = text(&app, width, height);
            assert!(rendered.contains(state), "{width}x{height}: {rendered}");
            assert!(rendered.contains("Esc Back"), "{rendered}");
            assert!(rendered.contains("f full-screen"), "{rendered}");
            assert_eq!(
                app.handle_key(KeyCode::Char('f'), KeyModifiers::NONE),
                Effect::FullscreenTerminal
            );
        }
    }
}

#[test]
fn hidden_terminal_releases_input_focus() {
    use crossterm::event::{Event, KeyEvent};
    let mut app = project_app(vec![project_source("/one", vec![row("work", "executing")])]);
    app.set_terminal(terminal_display());
    app.pane = app::Pane::Detail;
    for (width, height) in [(59, 24), (80, 14), (80, 24)] {
        app.resize(width, height);
        if width == 80 && height == 24 {
            app.handle_key(KeyCode::Left, KeyModifiers::NONE);
        }
        assert!(!app.terminal_visible());
        assert!(
            terminal_input_event(
                &app,
                Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE))
            )
            .is_none()
        );
        assert!(terminal_input_event(&app, Event::Paste("hidden".into())).is_none());
    }
    #[cfg(feature = "test-support")]
    with_terminal_client("hidden_terminal_releases_input_focus", || {
        use crate::terminal_sessions::{
            ControlOutcome, TerminalIdentity, TerminalSession, resolve,
        };
        let root = tempfile::tempdir().unwrap();
        let mut item = crate::work_model::WorkItem::planned("work", "Hidden control");
        item.add_initial_attempt("attempt-1").unwrap();
        let task = item.attempts[0].tasks[0].id.clone();
        crate::work_model::WorkModelStore::new(root.path())
            .create_work_item(&item)
            .unwrap();
        let mut session = TerminalSession::start_fixture(
            TerminalIdentity::fixture(root.path(), "work", "attempt-1", &task),
            "sleep 30",
        )
        .unwrap();
        let mut app = project_app(vec![project::SourceSnapshot::load(root.path()).unwrap()]);
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        app.handle_key(KeyCode::Down, KeyModifiers::NONE);
        let mut embedded = None;
        handle_terminal_key(
            &mut app,
            &mut embedded,
            KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE),
            |_| Ok(()),
        );
        handle_terminal_key(
            &mut app,
            &mut embedded,
            KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE),
            |_| Ok(()),
        );
        assert_terminal_client_ready(embedded.as_mut().unwrap());
        assert!(embedded.as_ref().unwrap().focused());
        let mut competing = resolve(root.path(), &session.descriptor().session_id).unwrap();
        assert_eq!(
            competing.acquire_control().unwrap(),
            ControlOutcome::Occupied
        );
        resize_terminal(&mut app, embedded.as_mut(), 59, 24);
        assert!(!embedded.as_ref().unwrap().focused());
        assert!(embedded.as_ref().unwrap().has_control());
        resize_terminal(&mut app, embedded.as_mut(), 80, 24);
        assert!(!embedded.as_ref().unwrap().focused());
        assert!(
            embedded
                .as_mut()
                .unwrap()
                .send(Event::Paste("hidden".into()))
                .is_err()
        );
        assert_eq!(
            competing.acquire_control().unwrap(),
            ControlOutcome::Occupied
        );
        drop(embedded);
        assert_eq!(
            competing.acquire_control().unwrap(),
            ControlOutcome::Acquired
        );
        drop(competing);
        session.stop().unwrap();
    });
}

#[test]
fn project_views_keep_large_counts_and_detail_overflow_visible() {
    let mut app = project_app(vec![project_source(
        "/one",
        (0..120)
            .map(|i| row(&format!("work-{i}"), "merged"))
            .collect(),
    )]);
    app.resize(60, 15);
    app.handle_key(KeyCode::BackTab, KeyModifiers::NONE);
    assert!(
        text(&app, 60, 15)
            .lines()
            .next()
            .unwrap()
            .contains("History 120")
    );
    app.snapshot.sources[0].diagnostics =
        vec!["a long diagnostic with many words ".repeat(70) + "END-DIAGNOSTIC"];
    app.handle_key(KeyCode::Right, KeyModifiers::NONE);
    for _ in 0..300 {
        app.handle_key(KeyCode::PageDown, KeyModifiers::NONE);
    }
    let rendered = text(&app, 60, 15);
    assert!(
        rendered
            .lines()
            .map(str::trim)
            .collect::<String>()
            .contains("END-DIAGNOSTIC"),
        "{rendered}"
    );
}

/// Drive the production coordinator while a real Work collection read waits on a FIFO.
struct StartupSession {
    fifo: PathBuf,
    writer: Option<std::fs::File>,
    keys: std::collections::VecDeque<KeyCode>,
    frames: Vec<String>,
}
impl DashboardSession for StartupSession {
    fn size(&self) -> Result<(u16, u16)> {
        Ok((140, 24))
    }
    fn draw(&mut self, app: &App) -> Result<()> {
        self.frames.push(text(app, 140, 24));
        Ok(())
    }
    fn next_event(&mut self, _: Duration) -> Result<Option<CEvent>> {
        use std::os::unix::fs::OpenOptionsExt;
        if self.writer.is_none() {
            assert!(self.frames[0].lines().next().unwrap().ends_with("Fluent"));
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                match std::fs::OpenOptions::new()
                    .write(true)
                    .custom_flags(libc::O_NONBLOCK)
                    .open(&self.fifo)
                {
                    Ok(writer) => {
                        self.writer = Some(writer);
                        break;
                    }
                    Err(error) if error.raw_os_error() == Some(libc::ENXIO) => {
                        assert!(Instant::now() < deadline, "reader never reached Work FIFO");
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("opening FIFO writer: {error}"),
                }
            }
        }
        Ok(Some(CEvent::Key(
            self.keys.pop_front().expect("quit was not handled").into(),
        )))
    }
    fn fullscreen(&mut self, _: &mut crate::terminal_view::EmbeddedTerminal) -> Result<()> {
        unreachable!()
    }
    fn run_action(
        &mut self,
        _: &actions::LocalCommand,
        _: Option<&mut crate::terminal_view::EmbeddedTerminal>,
    ) -> Result<actions::CommandOutcome> {
        unreachable!()
    }
    fn is_active(&self) -> bool {
        true
    }
    fn set_mouse_enabled(&mut self, _: bool) -> Result<()> {
        Ok(())
    }
}

#[test]
fn initial_local_read_does_not_block_first_draw() {
    let root = tempfile::tempdir().unwrap();
    let items = root.path().join(".fluent/work/items");
    std::fs::create_dir_all(&items).unwrap();
    let fifo = items.join("blocked.json");
    let name = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let mut session = StartupSession {
        fifo,
        writer: None,
        keys: [
            KeyCode::Tab,
            KeyCode::Char('?'),
            KeyCode::Char('?'),
            KeyCode::Char('q'),
        ]
        .into(),
        frames: vec![],
    };
    let mut app = loading_project_app();
    let mut local = LocalRefresh::new(root.path(), &[], root.path());
    let mut remote = crate::dashboard_sources::RemoteSources::new(vec![], Default::default());
    run_project_loop(&mut session, &mut app, &mut local, &mut remote).unwrap();
    assert!(app.should_quit());
    assert_eq!(app.filter, project::Filter::NeedsYou);
    assert!(
        session
            .frames
            .iter()
            .any(|frame| frame.contains("?/Esc close help"))
    );
    assert!(!local.pending.as_ref().unwrap().is_finished());
    // Exit must detach the still-blocked reader, before EOF can release it.
    drop(local);
    drop(session.writer.take());
}

#[test]
fn initial_snapshot_preserves_view_and_remote_updates() {
    let root = tempfile::tempdir().unwrap();
    let mut app = loading_project_app();
    let mut local = LocalRefresh::new(root.path(), &[], root.path());
    let next = project_app(vec![project_source(
        "/local",
        vec![row("local", "executing")],
    )])
    .snapshot;
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    local.start(&mut app, move || {
        started_tx.send(()).unwrap();
        release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        next
    });
    started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let mut remote = project_source("ssh:remote", vec![row("remote", "executing")]);
    apply_remote_results(&mut app, vec![remote.clone()]);
    assert!(text(&app, 140, 24).contains("remote title"));
    app.handle_key(KeyCode::Tab, KeyModifiers::NONE);
    app.handle_key(KeyCode::Tab, KeyModifiers::NONE);
    app.select_key(project::RowKey::work("ssh:remote", "remote"));
    let selected = app.selected_key.clone();
    remote.work[0].status.title = "New remote state".into();
    remote.diagnostics.push("Remote item diagnostic".into());
    apply_remote_results(&mut app, vec![remote.clone()]);
    assert!(!local.poll(&mut app));
    assert!(text(&app, 140, 24).contains("New remote state"));
    release_tx.send(()).unwrap();
    finish_local_refresh(&mut local, &mut app);
    assert_eq!(app.selected_key, selected);
    assert_eq!(app.filter, project::Filter::Running);
    assert_eq!(app.snapshot.sources[1], remote);
    assert!(
        app.snapshot
            .errors
            .iter()
            .any(|error| error.contains("Remote item diagnostic"))
    );
    assert!(text(&app, 140, 24).contains("> New remote state"));
    assert!(text(&app, 140, 24).contains("local title"));
}

#[test]
fn refresh_health_distinguishes_loading_success_and_error() {
    let parent = tempfile::tempdir().unwrap();
    let parent_path = parent.path().canonicalize().unwrap();
    // Place STALE across a display-row boundary regardless of the host's temp path.
    let parent_width = unicode_width::UnicodeWidthStr::width(parent_path.to_str().unwrap());
    let prefix_len = (156 + 160 - (parent_width + 1 + 6) % 160) % 160;
    let root = tempfile::Builder::new()
        .prefix(&"p".repeat(prefix_len))
        .rand_bytes(6)
        .tempdir_in(parent_path)
        .unwrap();
    let id = root
        .path()
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let mut app = loading_project_app();
    let mut local = LocalRefresh::new(root.path(), &[], root.path());
    assert!(
        text(&app, 160, 30)
            .lines()
            .next()
            .unwrap()
            .ends_with("Fluent")
    );
    // A collection failure on the first read must not masquerade as empty success.
    let items = root.path().join(".fluent/work/items");
    std::fs::create_dir_all(items.parent().unwrap()).unwrap();
    std::fs::write(&items, "obstruct enumeration").unwrap();
    local.request(&mut app, false);
    finish_local_refresh(&mut local, &mut app);
    assert!(app.source_reads[&id].error.is_some());
    assert_eq!(app.source_reads[&id].last_success, None);
    assert_eq!(app.local_read.last_success, None);
    assert!(!app.source_reads[&id].pending);
    assert!(text(&app, 160, 30).contains("last good Work"));
    std::fs::remove_file(&items).unwrap();
    let store = crate::work_model::WorkModelStore::new(root.path());
    store
        .create_work_item_with_transition(
            &crate::work_model::WorkItem::planned("work", "Keep good Work"),
            &crate::work_model::DurableTransitionMetadata {
                origin: Default::default(),
                reason: None,
                evidence_ref: None,
            },
        )
        .unwrap();
    local.request(&mut app, true);
    finish_local_refresh(&mut local, &mut app);
    let observed = app.source_reads[&id].last_success.unwrap();
    let updated = app.snapshot.sources[0].work[0].updated_at.clone();
    assert!(updated.is_some());
    assert!(app.source_reads[&id].error.is_none());
    assert!(!text(&app, 160, 30).contains("Last read UTC"));
    assert!(text(&app, 160, 30).contains("Updated UTC"));
    // Equal source values still record a new successful read and request a draw.
    app.take_dirty();
    local.request(&mut app, false);
    assert!(!text(&app, 160, 30).contains("Refreshing"));
    assert_eq!(app.source_reads[&id].last_success, Some(observed));
    finish_local_refresh(&mut local, &mut app);
    assert!(app.source_reads[&id].last_success.unwrap() > observed);
    assert_eq!(app.snapshot.sources[0].work[0].updated_at, updated);
    assert!(app.take_dirty());
    let observed = app.source_reads[&id].last_success;
    std::fs::remove_dir_all(&items).unwrap();
    std::fs::write(&items, "obstruct enumeration again").unwrap();
    local.request(&mut app, true);
    finish_local_refresh(&mut local, &mut app);
    assert_eq!(app.source_reads[&id].last_success, observed);
    assert!(app.source_reads[&id].error.is_some());
    apply_remote_results(&mut app, vec![project_source("ssh:remote", vec![])]);
    assert!(app.source_reads[&id].error.is_some());
    assert_eq!(app.source_reads[&id].last_success, observed);
    assert_eq!(
        app.snapshot.sources[0].work[0].status.title,
        "Keep good Work"
    );
    app.resize(160, 30);
    app.handle_key(KeyCode::Char('e'), KeyModifiers::NONE);
    let areas = layout::project_areas(ratatui::layout::Rect::new(0, 0, 160, 30));
    let mut diagnostic = String::new();
    loop {
        // Diagnostic words can cross rendered rows, and long paths can add pages.
        let rendered = text(&app, 160, 30);
        diagnostic.extend(
            rendered
                .lines()
                .skip(usize::from(areas.body.y + 1))
                .take(usize::from(areas.body.height - 1))
                .flat_map(str::chars)
                .filter(|character| !character.is_whitespace()),
        );
        let before = app.error_scroll;
        app.handle_key(KeyCode::PageDown, KeyModifiers::NONE);
        if app.error_scroll == before {
            break;
        }
    }
    assert!(diagnostic.contains("STALE"), "{diagnostic}");
    let error = app.source_reads[&id]
        .error
        .as_ref()
        .unwrap()
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    assert!(diagnostic.contains(&error), "{diagnostic}");
    assert!(diagnostic.contains("LastreadUTC"), "{diagnostic}");
    assert!(
        diagnostic.contains(&observed.unwrap().to_rfc3339()),
        "{diagnostic}"
    );
    app.handle_key(KeyCode::Char('e'), KeyModifiers::NONE);
    // A successful partial snapshot clears the read error and keeps diagnostics.
    std::fs::remove_file(&items).unwrap();
    store
        .create_work_item(&crate::work_model::WorkItem::planned(
            "work",
            "Recovered Work",
        ))
        .unwrap();
    std::fs::write(items.join("malformed.json"), "broken").unwrap();
    local.request(&mut app, true);
    finish_local_refresh(&mut local, &mut app);
    assert!(app.source_reads[&id].error.is_none());
    assert!(app.source_reads[&id].last_success > observed);
    assert!(
        app.snapshot
            .errors
            .iter()
            .any(|error| error.contains("malformed.json"))
    );
    assert!(text(&app, 160, 30).contains("Recovered Work"));
    // A different source completing cannot erase local diagnostics or health.
    apply_remote_results(&mut app, vec![project_source("ssh:remote", vec![])]);
    assert!(
        app.snapshot
            .errors
            .iter()
            .any(|error| error.contains("malformed.json"))
    );
    assert!(app.source_reads[&id].error.is_none());
}

#[test]
fn remote_read_health_tracks_receipts_independently() {
    let mut app = loading_project_app();
    let mut failed = project_source("ssh:failed", vec![row("retained", "executing")]);
    let healthy = project_source("ssh:healthy", vec![]);
    apply_remote_results(&mut app, vec![failed.clone(), healthy.clone()]);
    let observed = app.source_reads["ssh:failed"].last_success;
    assert!(observed.is_some());
    assert!(app.snapshot.sources[0].work[0].updated_at.is_none());
    failed.stale = Some("host unavailable".into());
    apply_remote_results(&mut app, vec![failed]);
    apply_remote_results(&mut app, vec![healthy]);
    assert_eq!(app.source_reads["ssh:failed"].last_success, observed);
    assert_eq!(
        app.source_reads["ssh:failed"].error.as_deref(),
        Some("host unavailable")
    );
    assert!(app.source_reads["ssh:healthy"].error.is_none());
    assert!(
        app.snapshot
            .errors
            .iter()
            .any(|error| error.contains("host unavailable"))
    );
    assert!(text(&app, 160, 30).contains("last good Work"));
}

#[test]
fn source_diagnostics_remain_scoped_when_remote_paths_match() {
    let mut app = loading_project_app();
    let mut first = project_source("ssh:first", vec![]);
    let mut second = project_source("ssh:second", vec![]);
    for source in [&mut first, &mut second] {
        source.path = "/same/path".into();
        source.diagnostics.push("Unreadable item".into());
    }
    apply_remote_results(&mut app, vec![first, second.clone()]);
    second.diagnostics.clear();
    apply_remote_results(&mut app, vec![second]);
    assert_eq!(app.snapshot.errors.len(), 1);
    assert!(app.snapshot.errors[0].contains("ssh:first"));
}

#[test]
fn dashboard_gutters_preserve_content_and_controls() {
    use ratatui::layout::Rect;
    let mut app = project_app(vec![project_source("/one", vec![row("work", "executing")])]);
    for (width, height) in [(60, 15), (99, 18), (100, 24), (140, 28)] {
        app.resize(width, height);
        let areas = layout::project_areas(Rect::new(0, 0, width, height));
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| render::draw(frame, &app)).unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = buffer_text(buffer);
        assert!(rendered.contains("work"), "{rendered}");
        assert!(rendered.contains("q quit"), "{rendered}");
        assert!(rendered.contains("Open Work Item"), "{rendered}");
        assert!(!rendered.contains('│'));
        app.set_terminal(terminal_display());
        terminal.draw(|frame| render::draw(frame, &app)).unwrap();
        let buffer = terminal.backend().buffer();
        if width >= 100 {
            assert_eq!(areas.body.y, areas.header.bottom());
            assert_eq!(areas.list.right() + 1, areas.divider.x);
            assert_eq!(areas.divider.right() + 1, areas.detail.x);
            for y in areas.body.y..areas.body.bottom() {
                assert_eq!(buffer[(areas.divider.x - 1, y)].symbol(), " ");
                assert_eq!(buffer[(areas.divider.right(), y)].symbol(), " ");
                assert_eq!(buffer[(areas.divider.x, y)].symbol(), "│");
            }
            assert!(areas.detail.right() <= width);
        }
        app.set_terminal(terminal_display());
        app.pane = app::Pane::Detail;
        let content = layout::terminal_content(app.size, &app.terminal.as_ref().unwrap().identity);
        assert_eq!(
            layout::terminal_size(app.size, &app.terminal.as_ref().unwrap().identity),
            (content.width, content.height)
        );
        assert!(content.x >= areas.detail.x && content.right() <= areas.detail.right());
        app.terminal = None;
        app.pane = app::Pane::List;
    }
}

#[test]
fn project_details_wrap_full_context_to_the_last_line() {
    for (width, height) in [(60, 15), (99, 18), (100, 24), (140, 28)] {
        let mut status = row("work", "needs-user");
        status.title = "Full title 漢字 e\u{301} ".repeat(10) + "TITLE-END";
        status.recovery_blocker = Some("Blocker 漢字 ".repeat(20) + "BLOCKER-END");
        status.next_action = Some("fluent work-item show ".repeat(30) + "COMMAND-END");
        let mut source = project_source("/one", vec![status]);
        source.work[0].workers.push(worker_row("/one", "writer"));
        for worker in [false, true] {
            let mut app = project_app(vec![source.clone()]);
            app.resize(width, height);
            if worker {
                app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
                app.handle_key(KeyCode::Down, KeyModifiers::NONE);
            }
            app.handle_key(KeyCode::Right, KeyModifiers::NONE);
            let areas = layout::project_areas(ratatui::layout::Rect::new(0, 0, width, height));
            let mut seen = String::new();
            for _ in 0..200 {
                let rendered = text(&app, width, height);
                for line in rendered
                    .lines()
                    .skip(areas.focused_detail.y as usize)
                    .take(areas.focused_detail.height as usize)
                {
                    seen.push_str(
                        &line
                            .chars()
                            .filter(|c| !c.is_whitespace())
                            .collect::<String>(),
                    );
                }
                app.handle_key(KeyCode::PageDown, KeyModifiers::NONE);
            }
            for expected in if worker {
                vec!["Source:"]
            } else {
                vec!["TITLE-END", "COMMAND-END", "Source:"]
            } {
                assert!(
                    seen.contains(expected),
                    "{width}, {worker}: missing {expected}"
                );
            }
            if worker {
                assert!(seen.contains("headlessexecution"));
                assert!(seen.contains("Invocation:generation-writer"));
            } else {
                assert!(seen.contains("BLOCKER-END"));
            }
            assert_eq!(app.detail_scroll, app.scroll_bounds().detail_max_scroll);
        }
    }
}

#[test]
fn dashboard_opens_on_current_project_work() {
    let history = project_source("/old", vec![row("past", "merged")]);
    let active = project_source(
        "ssh:remote",
        vec![
            row("unknown", "future"),
            row("ready", "task-ready"),
            row("running", "executing"),
            row("attention", "needs-user"),
        ],
    );
    let mut app = project_app(vec![history.clone(), active.clone()]);
    assert_eq!(app.filter.label(), "Current");
    assert_eq!(app.project_keys().len(), 4);
    assert_eq!(app.selected_id(), Some("attention"));
    assert!(text(&app, 140, 24).contains("Current 4"));
    let mut completed = active.clone();
    completed.work.pop();
    app.refresh(project_app(vec![history.clone(), completed.clone()]).snapshot);
    let mut fresh = project_app(vec![history.clone(), completed.clone()]);
    assert_eq!(fresh.selected_id(), Some("running"));
    completed.work.pop();
    fresh = project_app(vec![history.clone(), completed]);
    assert_eq!(fresh.selected_id(), Some("ready"));
    let mut empty = project_app(vec![history.clone()]);
    assert!(empty.selected_key.is_none());
    assert!(text(&empty, 100, 24).contains("Current 0"));
    empty.refresh(project_app(vec![history, active]).snapshot);
    assert_eq!(empty.selected_id(), Some("attention"));
    // A completed asynchronous refresh must retain and reveal a visible selection.
    empty.select_key(project::RowKey::work("ssh:remote", "running"));
    let selected = empty.selected_key.clone();
    let mut next = empty.snapshot.clone();
    next.sources[0].work = (0..30)
        .map(|i| {
            project_source("/old", vec![row(&format!("new-{i}"), "planned")])
                .work
                .remove(0)
        })
        .collect();
    let initially_selected = project_app(next.sources.clone());
    assert!(text(&initially_selected, 100, 24).contains("> attention"));
    empty.refresh(next);
    assert_eq!(empty.selected_key, selected);
    assert!(text(&empty, 100, 24).contains("> running"));
    empty.handle_key(KeyCode::BackTab, KeyModifiers::SHIFT);
    assert_eq!(empty.filter, project::Filter::History);
    assert!(empty.project_keys().is_empty());
    empty.handle_key(KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(empty.filter, project::Filter::Current);
}

#[test]
fn dashboard_focus_and_footer_follow_input_owner() {
    for (width, height) in [(60, 15), (99, 18), (100, 24), (140, 28)] {
        let mut app = project_app(vec![project_source("/one", vec![row("work", "executing")])]);
        app.resize(width, height);
        let list = text(&app, width, height);
        assert!(list.lines().next().unwrap().ends_with("Fluent"), "{list}");
        assert!(list.lines().last().unwrap().contains("q quit"));
        assert!(list.lines().last().unwrap().contains("Open Work Item"));
        app.handle_key(KeyCode::Right, KeyModifiers::NONE);
        let detail = text(&app, width, height);
        assert!(detail.contains("Work Item overview"), "{detail}");
        assert!(
            detail.lines().last().unwrap().contains("↑/↓ workers"),
            "{detail}"
        );
        assert!(
            detail.lines().last().unwrap().contains("Esc Back"),
            "{detail}"
        );
        assert!(app.project_controls().contains(&"Enter Open worker"));
        app.handle_key(KeyCode::Left, KeyModifiers::NONE);
        assert_eq!(app.pane, app::Pane::List);
        app.handle_key(KeyCode::Char('t'), KeyModifiers::NONE);
        app.set_terminal(terminal_display());
        app.pane = app::Pane::Detail;
        let observe = text(&app, width, height);
        assert!(
            observe.lines().last().unwrap().contains("i Control"),
            "{observe}"
        );
        assert!(
            observe.lines().last().unwrap().contains("Esc Back"),
            "{observe}"
        );
        assert_eq!(
            app.handle_key(KeyCode::Char('i'), KeyModifiers::NONE),
            Effect::ControlTerminal
        );
        app.handle_key(KeyCode::Char('?'), KeyModifiers::NONE);
        let help = text(&app, width, height);
        assert!(help.contains("f full-screen"), "{help}");
        assert!(help.lines().last().unwrap().contains("q quit"), "{help}");
        app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        app.handle_key(KeyCode::Char('e'), KeyModifiers::NONE);
        let errors = text(&app, width, height);
        assert!(
            errors.lines().last().unwrap().contains("e/Esc back"),
            "{errors}"
        );
        assert!(!errors.lines().last().unwrap().contains("i Control"));
        app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(!app.errors);
    }
    #[cfg(feature = "test-support")]
    with_terminal_client("dashboard_focus_and_footer_follow_input_owner", || {
        use crate::terminal_sessions::{TerminalIdentity, TerminalSession};
        use crossterm::event::{Event, KeyEvent};
        for (width, height) in [(60, 15), (99, 18), (100, 24), (240, 28)] {
            let root = tempfile::tempdir().unwrap();
            let mut item = crate::work_model::WorkItem::planned("work", "Control hints");
            item.add_initial_attempt("attempt-1").unwrap();
            let task = item.attempts[0].tasks[0].id.clone();
            crate::work_model::WorkModelStore::new(root.path())
                .create_work_item(&item)
                .unwrap();
            let mut session = TerminalSession::start_fixture(
                TerminalIdentity::fixture(root.path(), "work", "attempt-1", &task),
                "sleep 30",
            )
            .unwrap();
            let mut app = project_app(vec![project::SourceSnapshot::load(root.path()).unwrap()]);
            app.resize(width, height);
            app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
            app.handle_key(KeyCode::Down, KeyModifiers::NONE);
            let mut embedded = None;
            handle_terminal_key(
                &mut app,
                &mut embedded,
                KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE),
                |_| Ok(()),
            );
            assert_terminal_client_ready(embedded.as_mut().unwrap());
            let selected = app.selected_key.clone();
            let invocation = embedded.as_ref().unwrap().descriptor().clone();
            app.set_terminal(embedded.as_ref().unwrap().display());
            assert!(!embedded.as_ref().unwrap().has_control());
            assert!(embedded.as_mut().unwrap().send_literal_escape().is_err());
            for pane in [app::Pane::Detail, app::Pane::List] {
                app.pane = pane;
                assert!(
                    !app.project_controls().contains(&"] literal"),
                    "Observe {pane:?}"
                );
                assert!(
                    !text(&app, width, height)
                        .lines()
                        .last()
                        .unwrap()
                        .contains("] literal")
                );
                app.handle_key(KeyCode::Char('?'), KeyModifiers::NONE);
                let help = text(&app, width, height);
                assert!(!help.contains("] literal"), "Observe {width}: {help}");
                app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
            }
            assert_eq!(
                handle_terminal_key(
                    &mut app,
                    &mut embedded,
                    KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE),
                    |_| Ok(())
                ),
                Effect::ControlTerminal
            );
            // Control reconnects the client. Wait for its new paint rather than
            // accepting the Observe frame retained from the previous client.
            let deadline = Instant::now() + Duration::from_secs(5);
            let terminal = embedded.as_mut().unwrap();
            while !(terminal.refresh() && terminal.frame().text().contains("SSH_FULLSCREEN_PAINT"))
            {
                assert!(Instant::now() < deadline, "Control client did not paint");
                std::thread::sleep(Duration::from_millis(10));
            }

            assert!(embedded.as_ref().unwrap().has_control());
            let return_to_navigation =
                |app: &mut App, embedded: &mut Option<crate::terminal_view::EmbeddedTerminal>| {
                    let terminal = embedded.as_mut().unwrap();
                    terminal
                        .send(Event::Key(KeyEvent::new(
                            KeyCode::Char(']'),
                            KeyModifiers::CONTROL,
                        )))
                        .unwrap();
                    assert!(!terminal.focused());
                    assert!(terminal.has_control());
                    app.set_terminal(terminal.display());
                };
            return_to_navigation(&mut app, &mut embedded);
            let input_log =
                PathBuf::from(std::env::var_os("FLUENT_TEST_TERMINAL_CLIENT_LOG").unwrap());
            for pane in [app::Pane::Detail, app::Pane::List] {
                app.pane = pane;
                assert!(
                    app.project_controls().contains(&"] literal"),
                    "Control {pane:?}"
                );
                if width == 240 {
                    assert!(
                        text(&app, width, height)
                            .lines()
                            .last()
                            .unwrap()
                            .contains("] literal")
                    );
                }
                app.handle_key(KeyCode::Char('?'), KeyModifiers::NONE);
                let help = text(&app, width, height);
                assert!(help.contains("] literal"), "Control {width}: {help}");
                app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
                let received = || {
                    std::fs::read(&input_log)
                        .unwrap_or_default()
                        .iter()
                        .filter(|&&byte| byte == 29)
                        .count()
                };
                let before = received();
                assert_eq!(
                    handle_terminal_key(
                        &mut app,
                        &mut embedded,
                        KeyEvent::new(KeyCode::Char(']'), KeyModifiers::NONE),
                        |_| Ok(())
                    ),
                    Effect::LiteralEscape
                );
                assert!(app.stale_error.is_none(), "{:?}", app.stale_error);
                assert!(embedded.as_ref().unwrap().focused());
                let deadline = Instant::now() + Duration::from_secs(5);
                while received() == before {
                    assert!(
                        Instant::now() < deadline,
                        "literal chord did not reach the Control client"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
                assert_eq!(app.selected_key, selected);
                assert_eq!(embedded.as_ref().unwrap().descriptor(), &invocation);
                return_to_navigation(&mut app, &mut embedded);
            }
            assert_eq!(
                handle_terminal_key(
                    &mut app,
                    &mut embedded,
                    KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE),
                    |_| Ok(())
                ),
                Effect::ObserveTerminal
            );
            assert!(!embedded.as_ref().unwrap().has_control());
            assert!(!app.project_controls().contains(&"] literal"));
            app.handle_key(KeyCode::Char('?'), KeyModifiers::NONE);
            assert!(!text(&app, width, height).contains("] literal"));
            drop(embedded);
            session.stop().unwrap();
        }
    });
}

#[test]
fn focused_control_only_advertises_native_return_path() {
    for (width, height) in [(60, 15), (100, 24), (140, 28)] {
        let mut app = project_app(vec![project_source("/one", vec![row("work", "executing")])]);
        app.resize(width, height);
        app.pane = app::Pane::Detail;
        let mut display = terminal_display();
        display.focused = true;
        display.has_control = true;
        display.state = "Control · Ctrl-] navigate".into();
        app.set_terminal(display);
        let rendered = text(&app, width, height);
        assert!(rendered.contains("Control · Ctrl-] navigate"));
        assert_eq!(rendered.lines().last().unwrap().trim(), "Ctrl-] navigate");
        app.terminal.as_mut().unwrap().focused = false;
        app.terminal.as_mut().unwrap().state = "Control · navigation".into();
        let navigation = text(&app, width, height);
        assert!(navigation.contains("Control · navigation"));
        assert!(navigation.lines().last().unwrap().contains("q quit"));
    }
}

#[test]
fn worker_details_hide_retained_terminal_until_explicit_open() {
    for width in [60, 100, 140] {
        let mut source = project_source("/one", vec![row("work", "executing")]);
        source.work[0].workers.push(worker_row("/one", "writer"));
        let mut app = project_app(vec![source]);
        app.resize(width, 24);
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        app.handle_key(KeyCode::Down, KeyModifiers::NONE);
        app.handle_key(KeyCode::Char('t'), KeyModifiers::NONE);
        app.set_terminal(terminal_display());
        let selected = app.selected_key.clone();
        assert!(text(&app, width, 24).contains("native prompt"));
        assert_eq!(
            app.handle_key(KeyCode::Right, KeyModifiers::NONE),
            Effect::None
        );
        let details = text(&app, width, 24);
        assert!(details.contains("Worker: writer"), "{details}");
        assert!(!details.contains("native prompt"), "{details}");
        assert!(!app.terminal_visible());
        // Keep the attachment and invocation; only an explicit action reveals it.
        assert!(app.terminal.is_some());
        app.set_terminal(terminal_display());
        assert!(!app.terminal_visible());
        assert_eq!(app.selected_key, selected);
        assert_eq!(
            app.handle_key(KeyCode::Char('t'), KeyModifiers::NONE),
            Effect::OpenTerminal
        );
        assert!(app.terminal_visible());
    }
}

#[test]
fn project_source_headers_stay_quiet_during_polling() {
    let mut app = project_app(vec![project_source("/one", vec![row("work", "executing")])]);
    app.record_source_read(&app.snapshot.sources[0].clone());
    app.local_read.complete(None);
    let before = text(&app, 160, 30);
    app.begin_local_read();
    let during = text(&app, 160, 30);
    assert_eq!(before, during);
    assert!(!during.contains("Last read UTC"));
    assert!(!during.contains(&app.snapshot.sources[0].short_id()));
    let mut other = project_source("/two", vec![row("other", "executing")]);
    other.path = app.snapshot.sources[0].path.clone();
    app.snapshot.sources.push(other);
    let collided = text(&app, 160, 30);
    for source in &app.snapshot.sources {
        assert!(collided.contains(&source.short_id()));
    }
}

#[test]
fn project_read_progress_is_global_and_contextual() {
    let root = tempfile::tempdir().unwrap();
    let mut app = project_app(vec![]);
    app.local_read.complete(None);
    let mut local = LocalRefresh::new(root.path(), &[], root.path());
    local.request(&mut app, false);
    local.request(&mut app, true);
    let now = Instant::now();
    app.update_read_display(now);
    app.update_read_display(now + Duration::from_millis(600));
    assert_eq!(text(&app, 160, 30).matches("Reading sources").count(), 1);
    finish_local_refresh(&mut local, &mut app);
    app.update_read_display(now + Duration::from_millis(850));
    assert!(text(&app, 160, 30).contains("Reading sources"));
    finish_local_refresh(&mut local, &mut app);
    app.update_read_display(now + Duration::from_secs(1));
    assert!(!text(&app, 160, 30).contains("Reading sources"));
}

#[test]
fn project_source_read_warnings_preserve_work() {
    let mut app = project_app(vec![project_source(
        "/one",
        vec![row("retained", "executing")],
    )]);
    app.record_source_read(&app.snapshot.sources[0].clone());
    app.local_read.complete(None);
    app.begin_local_read();
    app.source_reads.get_mut("/one").unwrap().last_success =
        Some(chrono::Utc::now() - chrono::Duration::seconds(30));
    for width in [80, 160] {
        let rendered = text(&app, width, 30);
        assert!(rendered.contains("last good Work"));
        assert!(!rendered.contains("Last read UTC"));
        assert!(rendered.contains("retained"));
    }
    app.source_reads
        .get_mut("/one")
        .unwrap()
        .complete(Some("full diagnostic"));
    assert!(text(&app, 160, 30).contains("last good Work"));
    assert!(text(&app, 160, 30).contains("retained"));
    app.source_reads.clear();
    app.snapshot.sources[0].stale = Some("offline: full source diagnostic".into());
    app.snapshot.errors = project_diagnostics(&app.snapshot.sources);
    assert!(text(&app, 160, 30).contains("last good Work"));
    app.handle_key(KeyCode::Char('e'), KeyModifiers::NONE);
    assert!(text(&app, 160, 30).contains("offline: full source diagnostic"));
}

#[test]
fn right_arrow_opens_full_details_on_wide_dashboard() {
    for width in [60, 80, 100, 160] {
        for worker in [false, true] {
            let mut source = project_source("/one", vec![row("work", "executing")]);
            source.work[0].workers.push(worker_row("/one", "writer"));
            let mut app = project_app(vec![source]);
            app.resize(width, 24);
            if worker {
                app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
                app.handle_key(KeyCode::Down, KeyModifiers::NONE);
            }
            let before = text(&app, width, 24);
            if width >= 100 && !worker {
                assert!(!before.contains('│'));
            }
            app.handle_key(KeyCode::Right, KeyModifiers::NONE);
            let opened = text(&app, width, 24);
            assert!(
                opened.contains(if worker {
                    "Worker:"
                } else {
                    "Work Item overview"
                }),
                "{opened}"
            );
            assert!(!opened.contains('│'), "{opened}");
            assert!(!opened.contains("Updated UTC"));
            assert!(opened.contains("Work Item: work"));
        }
    }
}

#[test]
fn focused_details_return_preserves_selection_and_terminal_shortcut() {
    for width in [60, 100, 160] {
        let mut source = project_source(
            "/one",
            (0..25)
                .map(|i| row(&format!("item-{i}"), "planned"))
                .collect(),
        );
        source.work[24].status.id = "work".into();
        source.work[24].key = project::RowKey::work("/one", "work");
        source.work[24].status.next_action = Some("long next action ".repeat(100) + "ACTION-END");
        source.work[24].workers.push(worker_row("/one", "writer"));
        let mut app = project_app(vec![source]);
        app.resize(width, 18);
        for _ in 0..24 {
            app.handle_key(KeyCode::Down, KeyModifiers::NONE);
        }
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        let selected = app.selected_key.clone();
        let offset = app.list_scroll;
        let terminal_area = layout::terminal_content(app.size, "identity");
        for back in [KeyCode::Left, KeyCode::Esc] {
            assert_eq!(
                app.handle_key(KeyCode::Right, KeyModifiers::NONE),
                Effect::None
            );
            assert_eq!(
                layout::terminal_content(app.size, "identity"),
                terminal_area
            );
            if app.page == model::ProjectPage::Overview {
                app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
            }
            for _ in 0..300 {
                app.handle_key(KeyCode::Down, KeyModifiers::NONE);
            }
            assert_eq!(app.detail_scroll, app.scroll_bounds().detail_max_scroll);
            let values = app.detail_values();
            let body = layout::project_areas(ratatui::layout::Rect::new(0, 0, width, 18)).body;
            let expected = layout::project_detail_lines(&values, body.width)
                .len()
                .saturating_sub(body.height as usize);
            assert_eq!(app.detail_scroll as usize, expected);
            app.detail_scroll = 0;
            assert!(text(&app, width, 18).contains("headless execution"));
            assert_eq!(
                app.handle_key(KeyCode::Char('t'), KeyModifiers::NONE),
                Effect::OpenTerminal
            );
            app.handle_key(back, KeyModifiers::NONE);
            app.handle_key(back, KeyModifiers::NONE);
            app.handle_key(back, KeyModifiers::NONE);
            assert_eq!(app.selected_key, selected);
            assert_eq!(app.list_scroll, offset);
            assert!(text(&app, width, 18).contains("Updated UTC"));
        }
    }
}

#[test]
fn project_header_uses_one_read_status_and_right_brand() {
    let app = project_app(vec![project_source("/one", vec![row("work", "executing")])]);
    for width in [60, 160] {
        let rendered = text(&app, width, 24);
        let header = rendered.lines().next().unwrap();
        assert!(header.ends_with("Fluent"), "{rendered}");
        assert!(!header.contains("List"));
        assert!(!rendered.contains("Last read UTC"));
    }
}

#[test]
fn project_read_indicator_appears_only_for_visible_reads() {
    let mut app = loading_project_app();
    let start = Instant::now();
    app.update_read_display(start);
    assert!(!text(&app, 80, 24).contains("Reading sources"));
    app.update_read_display(start + Duration::from_millis(600));
    let first = text(&app, 80, 24);
    assert!(first.contains("Reading sources"));
    app.update_read_display(start + Duration::from_millis(850));
    let second = text(&app, 80, 24);
    assert_ne!(first.lines().nth(22), second.lines().nth(22));
    assert_eq!(
        first.lines().nth(22).unwrap().len(),
        second.lines().nth(22).unwrap().len()
    );
    app.local_read.complete(None);
    app.update_read_display(start + Duration::from_secs(1));
    app.begin_local_read();
    app.update_read_display(start + Duration::from_secs(2));
    assert!(!text(&app, 80, 24).contains("Reading sources"));
}

#[test]
fn project_source_warnings_use_one_global_status() {
    let mut app = project_app(vec![
        project_source("/one", vec![row("retained", "executing")]),
        project_source("/two", vec![]),
    ]);
    for source in app.snapshot.sources.clone() {
        app.record_source_read(&source);
    }
    app.source_reads.get_mut("/one").unwrap().last_success =
        Some(chrono::Utc::now() - chrono::Duration::seconds(30));
    app.source_reads.get_mut("/one").unwrap().pending = true;
    app.source_reads
        .get_mut("/two")
        .unwrap()
        .complete(Some("remote failed in detail"));
    let rendered = text(&app, 160, 30);
    assert!(
        rendered
            .lines()
            .nth(28)
            .unwrap()
            .contains("1 failed · 1 overdue"),
        "{rendered}"
    );
    assert_eq!(rendered.matches("! ").count(), 1);
    assert!(!rendered.contains("Last read UTC"));
    assert!(rendered.contains("retained"));
    app.handle_key(KeyCode::Char('e'), KeyModifiers::NONE);
    let diagnostics = text(&app, 160, 30);
    assert!(diagnostics.contains("remote failed in detail"));
    assert!(diagnostics.contains("Last read UTC"));
}

#[test]
fn project_views_exclude_redundant_all_and_preserve_membership() {
    use project::{Category as C, Filter};
    assert_eq!(
        Filter::VIEWS,
        [
            Filter::Current,
            Filter::NeedsYou,
            Filter::Running,
            Filter::Ready,
            Filter::Waiting,
            Filter::History
        ]
    );
    assert!(Filter::Current.matches(C::Unknown));
    assert!(!Filter::Current.matches(C::History));
    assert!(Filter::Ready.matches(C::ReadyToLand));
    assert!(Filter::Waiting.matches(C::Queued));
    let mut app = project_app(vec![project_source("/one", vec![row("work", "executing")])]);
    for _ in 0..6 {
        app.handle_key(KeyCode::Tab, KeyModifiers::NONE);
    }
    assert_eq!(app.filter, Filter::Current);
    app.handle_key(KeyCode::BackTab, KeyModifiers::SHIFT);
    assert_eq!(app.filter, Filter::History);
    app.handle_key(KeyCode::Char('a'), KeyModifiers::NONE);
    assert_eq!(app.filter, Filter::History);
}

#[test]
fn project_list_keeps_one_row_per_work_item() {
    let mut source = project_source(
        "/one",
        vec![row("work", "executing"), row("other", "planned")],
    );
    source.work[0].workers = (0..100)
        .map(|i| worker_row("/one", &format!("historical-{i}")))
        .collect();
    let mut app = project_app(vec![source]);
    assert_eq!(app.project_keys().len(), 2);
    for width in [60, 160] {
        let rendered = text(&app, width, 24);
        assert!(!rendered.contains("historical-"));
        assert!(!rendered.contains('↳'));
        assert_eq!(app.scroll_bounds().list_max_scroll, 0);
    }
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    for _ in 0..99 {
        app.handle_key(KeyCode::Down, KeyModifiers::NONE);
    }
    assert_eq!(
        app.selected_worker.as_ref().unwrap().task_id.as_deref(),
        Some("historical-99")
    );
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(
        app.detail_values()
            .join("\n")
            .contains("headless execution")
    );
    app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(app.project_keys().len(), 2);
}

#[test]
fn project_footer_shows_contextual_primary_controls() {
    let mut app = project_app(vec![project_source("/one", vec![row("work", "executing")])]);
    let controls = app.project_controls().join("; ");
    assert!(controls.contains("Tab/⇧Tab view"));
    for secondary in [
        "e Read issues",
        "r refresh",
        "c copy",
        "History/All",
        "F next view",
    ] {
        assert!(!controls.contains(secondary), "{controls}");
    }
    app.snapshot.errors.push("bad source".into());
    assert!(app.project_controls().contains(&"e Read issues"));
    app.handle_key(KeyCode::Char('?'), KeyModifiers::NONE);
    let help = text(&app, 60, 24);
    assert!(help.contains("text selection"), "{help}");
    assert!(
        help.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .contains("r refresh")
    );
    assert!(!help.contains("c copy"));
}

#[test]
fn project_details_open_summary_or_workers_and_return_to_selection() {
    for width in [60, 160] {
        let mut source = project_source(
            "/one",
            (0..30)
                .map(|i| row(&format!("work-{i}"), "executing"))
                .collect(),
        );
        let mut worker = worker_row("/one", "writer");
        worker.key.work_item_id = "work-29".into();
        source.work[29].workers = vec![worker];
        let mut app = project_app(vec![source]);
        app.resize(width, 24);
        for _ in 0..29 {
            app.handle_key(KeyCode::Down, KeyModifiers::NONE);
        }
        let key = app.selected_key.clone();
        let scroll = app.list_scroll;
        app.handle_key(KeyCode::Right, KeyModifiers::NONE);
        assert_eq!(app.page, model::ProjectPage::Overview);
        app.handle_key(KeyCode::Left, KeyModifiers::NONE);
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.page, model::ProjectPage::Overview);
        assert!(app.selected_worker.is_some());
        let rendered = text(&app, width, 24);
        assert!(rendered.contains("Workers"));
        assert!(
            rendered.lines().last().unwrap().contains("Open worker"),
            "{rendered}"
        );
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.page, model::ProjectPage::Worker);
        app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(text(&app, width, 24).contains("Workers"));
        app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(app.selected_key, key);
        assert_eq!(app.list_scroll, scroll);
    }
}

#[test]
fn detail_worker_selection_preserves_exact_terminal_target() {
    let mut source = project_source("/one", vec![row("work", "needs-user")]);
    source.work[0].workers = vec![worker_row("/one", "writer"), worker_row("/one", "reviewer")];
    let mut app = project_app(vec![source]);
    let parent = app.selected_key.clone();
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    let first = app.selected_worker.clone();
    app.handle_key(KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(
        app.selected_worker, first,
        "a visible parent keeps its worker across views"
    );
    app.handle_key(KeyCode::BackTab, KeyModifiers::SHIFT);
    assert_eq!(app.selected_worker, first);
    app.handle_key(KeyCode::Down, KeyModifiers::NONE);
    let second = app.selected_worker.clone();
    assert_ne!(first, second);
    assert_eq!(app.selected_key, parent);
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    assert!(text(&app, 160, 30).contains("headless execution"));
    let error = open_embedded(&app).err().unwrap().to_string();
    assert_eq!(error, "headless execution");
    let mut refreshed = app.snapshot.clone();
    refreshed.sources[0].work[0].workers.reverse();
    app.refresh(refreshed.clone());
    assert_eq!(app.selected_worker, second);
    refreshed.sources[0].work[0]
        .workers
        .retain(|worker| Some(&worker.key) != second.as_ref());
    app.refresh(refreshed);
    assert_eq!(app.selected_worker, None);
    assert_eq!(app.selected_key, parent);
    assert!(open_embedded(&app).is_err());
}

#[test]
fn project_preview_and_full_details_have_distinct_hierarchy() {
    let mut status = row("work", "merge-ready");
    status.title = "Full title with more context".into();
    status.merge_candidate = "candidate-7".into();
    let mut app = project_app(vec![project_source("/one", vec![status])]);
    let preview = text(&app, 160, 30);
    assert!(!preview.contains('│'));
    assert!(!preview.contains("fluent merge-candidate"));
    assert!(!preview.contains("Work Item:"));
    app.handle_key(KeyCode::Right, KeyModifiers::NONE);
    let full = app.detail_values().join("\n");
    assert!(full.starts_with("Full title with more context\nState:"));
    assert!(full.find("Next action").unwrap() < full.find("Workers (").unwrap());
    assert!(full.find("Workers (").unwrap() < full.find("Work Item:").unwrap());
    assert!(full.contains("Merge Candidate: candidate-7"));
}

#[test]
fn project_actions_keep_commands_readable_and_exact() {
    let mut status = row("20261106-dashboard-with-a-long-name", "merge-ready");
    status.merge_candidate = "attempt-1-merge-candidate".into();
    let app = project_app(vec![project_source("/one", vec![status.clone()])]);
    let values = app.detail_values();
    assert!(values.contains(&format!(
        "  Candidate show: fluent merge-candidate show {} {}",
        status.id, status.merge_candidate
    )));
    assert!(values.contains(&format!(
        "  Candidate land: fluent merge-candidate land {} {}",
        status.id, status.merge_candidate
    )));
    assert!(values.iter().any(|value| value.contains("Candidate show:")));
    assert!(values.iter().any(|value| value.contains("Candidate land:")));
    for command in values
        .iter()
        .filter(|value| value.starts_with("  Candidate"))
    {
        let lines = layout::project_detail_lines(std::slice::from_ref(command), 60);
        assert_eq!(
            lines
                .iter()
                .map(|line| line.trim())
                .collect::<Vec<_>>()
                .join(" "),
            command.trim()
        );
    }
    let guidance = vec![
        "Next action: Inspect the pending evidence carefully before resuming the selected attempt"
            .into(),
    ];
    let wrapped = layout::project_detail_lines(&guidance, 30);
    assert_eq!(
        wrapped
            .iter()
            .map(|line| line.trim())
            .collect::<Vec<_>>()
            .join(" "),
        guidance[0]
    );
    assert!(wrapped.iter().skip(1).all(|line| line.starts_with("  ")));
    for width in [1, 2, 17, 60, 160] {
        let wrapped = layout::project_detail_lines(&values, width);
        assert!(
            wrapped
                .iter()
                .all(|line| unicode_width::UnicodeWidthStr::width(line.as_str()) <= width as usize)
        );
    }
    let unicode = "漢字 e\u{301} words";
    let wrapped = layout::project_detail_lines(&[unicode.into()], 2);
    let compact = |value: &str| {
        value
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
    };
    assert_eq!(compact(&wrapped.join("")), compact(unicode));
    assert!(
        wrapped
            .iter()
            .all(|line| unicode_width::UnicodeWidthStr::width(line.as_str()) <= 2)
    );
    // Free-form guidance stays readable and never supplies command arguments.
    let mut custom = status;
    custom.next_action = Some("Inspect host evidence before any landing command".into());
    let app = project_app(vec![project_source("/one", vec![custom])]);
    let full = app.detail_values().join("\n");
    assert!(full.contains("Inspect host evidence before any landing command"));
    assert!(full.contains("Candidate land: fluent merge-candidate land"));
}

#[test]
fn project_list_hides_empty_groups_without_hiding_read_issues() {
    let mut hidden = project_source("/hidden", vec![row("old", "complete")]);
    hidden.project = "hidden-project".into();
    hidden.stale = Some("hidden read failed".into());
    let mut app = project_app(vec![
        hidden,
        project_source("/visible", vec![row("work", "executing")]),
    ]);
    app.snapshot.errors = project_diagnostics(&app.snapshot.sources);
    app.resize(60, 15);
    let rendered = text(&app, 60, 15);
    assert!(!rendered.contains("hidden-project"), "{rendered}");
    assert!(rendered.contains("1 failed"), "{rendered}");
    assert!(rendered.contains("Read issues"), "{rendered}");
    assert_eq!(app.scroll_bounds().list_max_scroll, 0);
    app.handle_key(KeyCode::Char('e'), KeyModifiers::NONE);
    assert!(text(&app, 60, 15).contains("hidden read failed"));
    app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
    app.filter = project::Filter::Ready;
    let empty = text(&app, 60, 15);
    assert!(empty.contains("No Work Items in this view"));
    assert!(!empty.contains("same-name"));
}

#[test]
fn project_list_uses_full_width_until_terminal_is_visible() {
    let mut app = project_app(vec![project_source("/one", vec![row("work", "executing")])]);
    app.snapshot.sources[0].work[0].status.title =
        "A Work Item title that occupies more than the former narrow list could show".into();
    app.resize(120, 24);
    let rendered = text(&app, 120, 24);
    assert!(
        rendered.contains("former narrow list could show"),
        "{rendered}"
    );
    assert!(!rendered.contains('│'));
    app.set_terminal(terminal_display());
    let split = text(&app, 120, 24);
    assert!(split.contains('│'));
    assert!(split.contains("same-name"));
    assert!(split.contains("Observe"));
    app.resize(60, 15);
    assert!(text(&app, 60, 15).contains("same-name"));
}

#[test]
fn project_footer_explains_source_health_and_read_issues() {
    let mut app = project_app(vec![project_source("/one", vec![row("work", "executing")])]);
    app.record_source_read(&app.snapshot.sources[0].clone());
    app.local_read.complete(None);
    for width in [60, 160] {
        let healthy = text(&app, width, 15);
        assert!(
            healthy
                .lines()
                .nth(13)
                .unwrap()
                .contains("Source reads: 1 healthy"),
            "{healthy}"
        );
        assert!(healthy.lines().next().unwrap().ends_with("Fluent"));
        assert_eq!(healthy.matches("Tab/⇧Tab view").count(), 1);
    }
    app.source_reads.get_mut("/one").unwrap().last_success =
        Some(chrono::Utc::now() - chrono::Duration::seconds(30));
    app.source_reads.get_mut("/one").unwrap().pending = true;
    let overdue = text(&app, 60, 15);
    assert!(
        overdue.lines().nth(13).unwrap().contains("1 overdue"),
        "{overdue}"
    );
    assert!(overdue.contains("last good"));
    app.source_reads
        .get_mut("/one")
        .unwrap()
        .complete(Some("full diagnostic"));
    assert!(text(&app, 60, 15).contains("1 failed"));
    app.handle_key(KeyCode::Char('e'), KeyModifiers::NONE);
    let issues = text(&app, 60, 15);
    assert!(issues.contains("full diagnostic"));
    assert!(issues.contains("Last read UTC"));
}

#[test]
fn project_read_activity_coexists_with_warnings_and_stays_quiet_on_poll() {
    let mut app = project_app(vec![project_source("/one", vec![row("work", "executing")])]);
    app.record_source_read(&app.snapshot.sources[0].clone());
    app.local_read.complete(None);
    app.source_reads
        .get_mut("/one")
        .unwrap()
        .complete(Some("offline"));
    app.begin_local_read();
    app.checking_sources = true;
    let now = Instant::now();
    app.update_read_display(now);
    assert!(!text(&app, 60, 15).contains("Reading sources"));
    app.update_read_display(now + Duration::from_millis(600));
    let first = text(&app, 60, 15);
    assert!(first.contains("Reading sources"), "{first}");
    assert!(first.contains("1 failed"));
    app.update_read_display(now + Duration::from_millis(850));
    assert_ne!(first, text(&app, 60, 15));
    app.source_reads.get_mut("/one").unwrap().complete(None);
    app.local_read.complete(None);
    app.update_read_display(now + Duration::from_secs(1));
    let idle = text(&app, 60, 15);
    app.begin_local_read();
    app.update_read_display(now + Duration::from_secs(2));
    assert_eq!(idle, text(&app, 60, 15));
}

#[test]
fn project_header_and_detail_breadcrumb_show_one_navigation_level() {
    for width in [60, 160] {
        let mut source = project_source("/one", vec![row("work", "executing")]);
        source.work[0].workers = vec![worker_row("/one", "writer")];
        let mut app = project_app(vec![source]);
        app.resize(width, 15);
        assert!(
            text(&app, width, 15)
                .lines()
                .next()
                .unwrap()
                .contains("Current 1")
        );
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        let overview = text(&app, width, 15);
        assert!(
            overview.lines().next().unwrap().contains("Work Item"),
            "{overview}"
        );
        assert!(!overview.contains("Current 1"));
        let filter = app.filter;
        app.handle_key(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.filter, filter);
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        let worker = text(&app, width, 15);
        assert!(
            worker.lines().next().unwrap().contains("Worker"),
            "{worker}"
        );
        assert!(worker.lines().last().unwrap().contains("Back"));
    }
}

#[test]
fn project_overview_shows_action_and_visible_worker_table() {
    for (width, height) in [(60, 15), (160, 30)] {
        let mut source = project_source("/one", vec![row("work", "executing")]);
        source.work[0].workers = (0..15)
            .map(|n| worker_row("/one", &format!("review-{n:02}")))
            .collect();
        source.work[0].workers[0].elapsed_ms = Some(42000);
        let mut app = project_app(vec![source]);
        app.resize(width, height);
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        let overview = text(&app, width, height);
        assert!(overview.contains("Next action"), "{overview}");
        assert!(overview.contains("Workers (15)"), "{overview}");
        assert!(overview.contains("> review-00"), "{overview}");
        if width >= 100 {
            assert!(overview.contains("42s"));
            assert!(overview.contains("Terminal"));
            assert!(overview.contains("review-14"));
        }
        for _ in 0..14 {
            app.handle_key(KeyCode::Down, KeyModifiers::NONE);
        }
        assert!(text(&app, width, height).contains("> review-14"));
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        let worker = text(&app, width, height);
        assert!(worker.contains("Worker: review-14"), "{worker}");
        assert!(worker.contains("headless execution"));
    }
}

#[test]
fn project_enter_opens_visible_hierarchy_and_preserves_selection() {
    for (width, height) in [(60, 15), (160, 24)] {
        let mut source = project_source("/one", vec![row("work", "executing")]);
        source.work[0].workers = (0..15)
            .map(|n| worker_row("/one", &format!("review-{n:02}")))
            .collect();
        let mut app = project_app(vec![source]);
        app.resize(width, height);
        app.handle_key(KeyCode::Right, KeyModifiers::NONE);
        for _ in 0..14 {
            app.handle_key(KeyCode::Down, KeyModifiers::NONE);
        }
        let selected = app.selected_worker.clone();
        assert!(text(&app, width, height).contains("> review-14"));
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert!(text(&app, width, height).contains("Worker: review-14"));
        app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(text(&app, width, height).contains("> review-14"));
        app.handle_key(KeyCode::Left, KeyModifiers::NONE);
        assert!(text(&app, width, height).contains("Open Work Item"));
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.selected_worker, selected);
        let reopened = text(&app, width, height);
        assert!(reopened.contains("> review-14"), "{reopened}");
        // Reordering must reveal the same worker at either end of the table.
        for _ in 0..2 {
            let mut snapshot = app.snapshot.clone();
            snapshot.sources[0].work[0].workers.reverse();
            app.refresh(snapshot);
            assert_eq!(app.selected_worker, selected);
            let refreshed = text(&app, width, height);
            assert!(refreshed.contains("> review-14"), "{refreshed}");
        }
        let mut snapshot = app.snapshot.clone();
        snapshot.sources[0].work[0].workers[14]
            .key
            .execution_generation = Some("replacement".into());
        app.refresh(snapshot);
        assert!(app.selected_worker.is_none());
    }
}

#[test]
fn worker_page_keeps_exact_terminal_identity_and_clear_unavailability() {
    let mut source = project_source("/one", vec![row("work", "executing")]);
    source.work[0].workers = vec![worker_row("/one", "writer"), worker_row("/one", "tester")];
    let mut app = project_app(vec![source]);
    app.set_terminal(terminal_display());
    let retained = app.terminal.as_ref().unwrap().identity.clone();
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    app.handle_key(KeyCode::Down, KeyModifiers::NONE);
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    let selected = app.selected_worker.clone();
    let rendered = text(&app, 100, 24);
    for field in [
        "Worker: tester",
        "State: executing",
        "Agent terminal: headless execution",
        "Provider:",
        "Elapsed:",
        "Technical identifiers",
        "Invocation: generation-tester",
    ] {
        assert!(rendered.contains(field), "missing {field}: {rendered}");
    }
    let mut embedded = None;
    handle_terminal_key(
        &mut app,
        &mut embedded,
        KeyCode::Char('t').into(),
        |_| unreachable!(),
    );
    assert_eq!(app.selected_worker, selected);
    assert_eq!(app.terminal.as_ref().unwrap().identity, retained);
    assert!(text(&app, 100, 24).contains("headless execution"));
}

fn worker_row(source: &str, task: &str) -> project::Worker {
    project::Worker {
        key: project::RowKey {
            source_id: source.into(),
            work_item_id: "work".into(),
            attempt_id: Some("attempt-1".into()),
            task_id: Some(task.into()),
            execution_generation: Some(format!("generation-{task}")),
        },
        role: task.into(),
        state: "executing".into(),
        provider: None,
        model: None,
        effort: None,
        elapsed_ms: None,
        updated_at: None,
        terminal: None,
        unavailable_reason: "headless execution".into(),
    }
}

#[test]
fn project_actions_are_typed_local_and_copy_only_remote() {
    use super::actions::{ActionKind, offered};
    for state in [
        "task-ready",
        "learner-not-ready",
        "merge-ready",
        "needs-user",
        "executing",
    ] {
        let mut status = row("work", state);
        status.attempt = "attempt-1".into();
        status.merge_candidate = "candidate-7".into();
        status.next_action = Some("touch /tmp/never-run-dashboard-guidance".into());
        for id in ["/local", "ssh:remote"] {
            let source = project_source(id, vec![status.clone()]);
            let actions = offered(&source, &source.work[0]);
            let kinds = actions.iter().map(|action| action.kind).collect::<Vec<_>>();
            assert_eq!(
                kinds,
                match state {
                    "task-ready" | "learner-not-ready" => vec![ActionKind::AttemptRun],
                    "merge-ready" => vec![ActionKind::CandidateShow, ActionKind::CandidateLand],
                    _ => vec![],
                }
            );
            for action in &actions {
                assert_eq!(action.remote(), id.starts_with("ssh:"));
                assert!(!action.command_text().contains("touch"));
                assert_eq!(action.args()[2], "work");
                assert_eq!(
                    action.args()[3],
                    if state == "merge-ready" {
                        "candidate-7"
                    } else {
                        "attempt-1"
                    }
                );
                assert!(action.copy_text().contains(id));
            }
            let mut app = project_app(vec![source]);
            app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
            app.handle_key(KeyCode::Char('a'), KeyModifiers::NONE);
            if !actions.is_empty() {
                let rendered = text(&app, 160, 30);
                assert!(rendered.contains(actions[0].kind.label()), "{rendered}");
                assert!(rendered.contains(&actions[0].command_text()));
                assert!(rendered.contains(id));
                if id.starts_with("ssh:") {
                    assert!(rendered.contains("Copy only"));
                    assert_eq!(
                        app.handle_key(KeyCode::Enter, KeyModifiers::NONE),
                        Effect::None
                    );
                    assert_eq!(
                        app.handle_key(KeyCode::Char('c'), KeyModifiers::NONE),
                        Effect::CopyAction
                    );
                }
            } else {
                assert!(text(&app, 160, 30).contains("touch /tmp/never-run-dashboard-guidance"));
            }
        }
    }
    let mut source = project_source("/local", vec![row("work", "merge-ready")]);
    source.work[0].status.merge_candidate = "-".into();
    assert!(
        offered(&source, &source.work[0]).is_empty(),
        "missing candidate is not an action"
    );
    source.work[0].status.merge_candidate = "--help".into();
    assert!(
        offered(&source, &source.work[0]).is_empty(),
        "identifiers cannot inject flags"
    );
}

fn action_fixture() -> (tempfile::TempDir, App) {
    action_fixture_in(tempfile::tempdir().unwrap())
}

fn action_fixture_in(root: tempfile::TempDir) -> (tempfile::TempDir, App) {
    crate::git::run(
        root.path(),
        &["init", "-b", "main"],
        "initialize action checkout",
    )
    .unwrap();
    let mut item = crate::work_model::WorkItem::planned("work", "Run this Attempt");
    item.add_initial_attempt("attempt-1").unwrap();
    crate::work_model::WorkModelStore::new(root.path())
        .create_work_item(&item)
        .unwrap();
    let mut app = project_app(vec![project::SourceSnapshot::load(root.path()).unwrap()]);
    app.resize(160, 30);
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    app.handle_key(KeyCode::Char('a'), KeyModifiers::NONE);
    (root, app)
}

#[test]
fn project_action_confirmation_rejects_stale_or_declined_command() {
    // A canonical Tester scratch path can exceed the viewport width. Make this
    // case independent of the ambient TMPDIR so every run exercises wrapping.
    let root = tempfile::Builder::new()
        .prefix(&"long-checkout-".repeat(14))
        .tempdir()
        .unwrap();
    let (root, mut app) = action_fixture_in(root);
    assert_eq!(
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE),
        Effect::PrepareAction
    );
    prepare_dashboard_action(&mut app);
    let confirmation = text(&app, 160, 30);
    for expected in [
        "Confirm local action",
        "fluent attempt run work attempt-1",
        "Work Item: work",
        "foreground",
        "y Confirm",
        "n Cancel",
    ] {
        assert!(
            confirmation.contains(expected),
            "missing {expected}: {confirmation}"
        );
    }
    let checkout = root.path().canonicalize().unwrap();
    assert_eq!(
        app.pending_action.as_ref().unwrap().action.checkout,
        checkout
    );
    // Ignore layout whitespace introduced by wrapping and padded terminal rows,
    // while requiring the complete checkout text in its original order.
    assert!(
        confirmation
            .split_whitespace()
            .collect::<String>()
            .contains(
                &checkout
                    .to_str()
                    .unwrap()
                    .split_whitespace()
                    .collect::<String>()
            ),
        "checkout missing from confirmation: {confirmation}"
    );
    assert_eq!(
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE),
        Effect::None
    );
    assert!(app.take_confirmed_action().is_none());
    app.handle_key(KeyCode::Char('n'), KeyModifiers::NONE);
    assert!(app.take_confirmed_action().is_none());
    assert!(text(&app, 160, 30).contains("Canceled"));
    prepare_dashboard_action(&mut app);
    let store = crate::work_model::WorkModelStore::new(root.path());
    let mut changed = store.read_work_item_snapshot("work").unwrap();
    changed.title = "Changed since confirmation".into();
    store.write_work_item(&changed).unwrap();
    assert_eq!(
        app.handle_key(KeyCode::Char('y'), KeyModifiers::NONE),
        Effect::RunAction
    );
    let action = app.take_confirmed_action().unwrap();
    assert!(
        action
            .validate()
            .unwrap_err()
            .to_string()
            .contains("changed")
    );
    assert!(app.take_confirmed_action().is_none());
    assert_eq!(
        app.handle_key(KeyCode::Char('y'), KeyModifiers::NONE),
        Effect::None
    );
    let (_, mut stale) = action_fixture();
    stale.snapshot.sources[0].stale = Some("offline".into());
    prepare_dashboard_action(&mut stale);
    assert!(stale.pending_action.is_none());
    assert!(text(&stale, 160, 30).contains("stale"));
    let (_, mut missing) = action_fixture();
    missing.snapshot.sources.clear();
    prepare_dashboard_action(&mut missing);
    assert!(missing.pending_action.is_none());
}

struct ActionSession {
    size: (u16, u16),
    calls: Vec<actions::LocalCommand>,
    outcome: Option<Result<actions::CommandOutcome>>,
}
impl DashboardSession for ActionSession {
    fn size(&self) -> Result<(u16, u16)> {
        Ok(self.size)
    }
    fn draw(&mut self, _: &App) -> Result<()> {
        Ok(())
    }
    fn next_event(&mut self, _: Duration) -> Result<Option<CEvent>> {
        unreachable!()
    }
    fn fullscreen(&mut self, _: &mut crate::terminal_view::EmbeddedTerminal) -> Result<()> {
        unreachable!()
    }
    fn is_active(&self) -> bool {
        true
    }
    fn set_mouse_enabled(&mut self, _: bool) -> Result<()> {
        Ok(())
    }
    fn run_action(
        &mut self,
        command: &actions::LocalCommand,
        _: Option<&mut crate::terminal_view::EmbeddedTerminal>,
    ) -> Result<actions::CommandOutcome> {
        self.calls.push(command.clone());
        self.outcome.take().unwrap()
    }
}

fn commit_action_candidate(root: &Path, message: &str) -> String {
    crate::git::run(
        root,
        &[
            "-c",
            "user.name=Dashboard tests",
            "-c",
            "user.email=dashboard@example.test",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-m",
            message,
        ],
        "commit action fixture",
    )
    .unwrap();
    crate::git::run_stdout(root, &["rev-parse", "HEAD"], "read action fixture commit")
        .unwrap()
        .trim()
        .into()
}

fn merge_ready_action_fixture() -> (tempfile::TempDir, App) {
    use crate::work_model::{
        AttemptLearning, AttemptReviewState, AttemptStatus, TaskOutput, TaskStatus,
    };
    let (root, _) = action_fixture();
    let commit = commit_action_candidate(root.path(), "Initial candidate");
    let store = crate::work_model::WorkModelStore::new(root.path());
    let mut item = store.read_work_item_snapshot("work").unwrap();
    let attempt = &mut item.attempts[0];
    let workspace = attempt.tasks[0].workspace_access.writes[0].clone();
    attempt.tasks[0].status = TaskStatus::Complete;
    attempt.tasks[0].output = Some(TaskOutput {
        workspace_id: workspace.id,
        workspace_path: workspace.path,
        source_branch: "candidate".into(),
        base_commit: None,
        commit,
        no_change: None,
        learner_canonicalization: None,
    });
    attempt.status = AttemptStatus::Complete;
    attempt.review_state = Some(AttemptReviewState::Passed);
    attempt.learning = Some(AttemptLearning::succeeded(
        1,
        crate::follow_up::ArtifactRef {
            path: "handoff.json".into(),
            digest: "sha256:fixture".into(),
        },
    ));
    // Status uses the persisted ordinary review bindings, not just the
    // Attempt's summary flag. Retained Git objects authenticate each review.
    item.add_review_tasks("attempt-1", crate::review::REVIEWERS)
        .unwrap();
    for task in item.attempts[0]
        .tasks
        .iter_mut()
        .filter(|task| task.kind == crate::work_model::TaskKind::Review)
    {
        task.status = TaskStatus::Complete;
        let area = root.path().join(&task.artifact_area.as_ref().unwrap().path);
        std::fs::create_dir_all(&area).unwrap();
        std::fs::write(area.join("review.md"), "Verdict: pass\n").unwrap();
    }
    item.attempts[0].status = AttemptStatus::Complete;
    item.attempts[0].review_state = Some(AttemptReviewState::Passed);
    item.create_or_get_merge_candidate("attempt-1").unwrap();
    store.write_work_item(&item).unwrap();
    let source = project::SourceSnapshot::load(root.path()).unwrap();
    assert_eq!(source.work[0].status.action, "merge-ready");
    let mut app = project_app(vec![source]);
    app.resize(160, 30);
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    app.handle_key(KeyCode::Char('a'), KeyModifiers::NONE);
    (root, app)
}

/// Drive the same action effects as the event loop, replacing only the runner.
fn dispatch_action_key(
    app: &mut App,
    session: &mut ActionSession,
    local: &mut LocalRefresh,
    key: KeyCode,
) {
    let effect = app.handle_key(key, KeyModifiers::NONE);
    if effect == Effect::PrepareAction {
        prepare_dashboard_action(app);
    }
    if matches!(effect, Effect::PrepareAction | Effect::RunAction) {
        run_dashboard_action(session, app, local, None).unwrap();
    }
}

#[test]
fn project_action_candidate_land_requires_confirmation() {
    for decline in [KeyCode::Char('n'), KeyCode::Esc] {
        let (root, mut app) = merge_ready_action_fixture();
        let store = crate::work_model::WorkModelStore::new(root.path());
        let original = store.read_work_item_snapshot("work").unwrap();
        let candidate = &original.merge_candidates[0].id;
        let mut session = ActionSession {
            size: (160, 30),
            calls: vec![],
            outcome: Some(Ok(actions::CommandOutcome::Succeeded)),
        };
        let mut local = LocalRefresh::new(root.path(), &[], root.path());
        dispatch_action_key(&mut app, &mut session, &mut local, KeyCode::Down);
        assert_eq!(
            app.action_menu.as_ref().unwrap().current().kind,
            actions::ActionKind::CandidateLand
        );
        assert!(session.calls.is_empty(), "selecting land must not launch");
        dispatch_action_key(&mut app, &mut session, &mut local, KeyCode::Enter);
        assert!(
            session.calls.is_empty(),
            "opening land confirmation must not launch"
        );
        assert!(app.pending_action.is_some());
        let confirmation = text(&app, 160, 30);
        for expected in [
            "Confirm local action",
            "Work Item: work",
            "Land this Merge Candidate",
            "y Confirm",
            "n Cancel",
        ] {
            assert!(confirmation.contains(expected), "{confirmation}");
        }
        assert!(confirmation.contains(&format!("fluent merge-candidate land work {candidate}")));
        let canonical = root.path().canonicalize().unwrap();
        assert!(
            confirmation
                .split_whitespace()
                .collect::<String>()
                .contains(
                    &canonical
                        .to_str()
                        .unwrap()
                        .split_whitespace()
                        .collect::<String>()
                )
        );
        dispatch_action_key(&mut app, &mut session, &mut local, KeyCode::Enter);
        assert!(
            session.calls.is_empty(),
            "Enter cannot approve a state-changing command"
        );
        assert!(app.pending_action.is_some());
        dispatch_action_key(&mut app, &mut session, &mut local, decline);
        assert!(session.calls.is_empty(), "declining must not launch");
        assert!(app.pending_action.is_none() && app.confirmed_action.is_none());
        assert!(app.action_message.as_ref().unwrap().contains("Canceled"));
        dispatch_action_key(&mut app, &mut session, &mut local, KeyCode::Char('y'));
        assert!(
            session.calls.is_empty(),
            "a declined confirmation cannot be reused"
        );

        dispatch_action_key(&mut app, &mut session, &mut local, KeyCode::Enter);
        assert!(session.calls.is_empty());
        dispatch_action_key(&mut app, &mut session, &mut local, KeyCode::Char('y'));
        assert_eq!(session.calls.len(), 1);
        assert_eq!(
            session.calls[0].executable,
            std::env::current_exe().unwrap()
        );
        assert_eq!(
            session.calls[0].args,
            ["merge-candidate", "land", "work", candidate]
        );
        assert_eq!(session.calls[0].checkout, canonical);
        dispatch_action_key(&mut app, &mut session, &mut local, KeyCode::Char('y'));
        run_dashboard_action(&mut session, &mut app, &mut local, None).unwrap();
        assert_eq!(session.calls.len(), 1, "one confirmation must launch once");
        assert!(
            app.action_message
                .as_ref()
                .unwrap()
                .contains("Candidate land succeeded")
        );
        assert!(local.pending.is_some());
        finish_local_refresh(&mut local, &mut app);
        assert_eq!(app.snapshot.sources[0].work[0].status.action, "merge-ready");
        assert_eq!(
            store.read_work_item_snapshot("work").unwrap(),
            original,
            "the fake runner must not land Work or predict its state"
        );
    }
}

#[test]
fn project_action_candidate_land_rechecks_persisted_candidate() {
    for before_confirmation in [true, false] {
        for field in ["identity", "commit"] {
            let (root, mut app) = merge_ready_action_fixture();
            let mut session = ActionSession {
                size: (160, 30),
                calls: vec![],
                outcome: Some(Ok(actions::CommandOutcome::Succeeded)),
            };
            let mut local = LocalRefresh::new(root.path(), &[], root.path());
            dispatch_action_key(&mut app, &mut session, &mut local, KeyCode::Down);
            if !before_confirmation {
                dispatch_action_key(&mut app, &mut session, &mut local, KeyCode::Enter);
                assert!(app.pending_action.is_some());
            }
            let store = crate::work_model::WorkModelStore::new(root.path());
            let mut changed = store.read_work_item_snapshot("work").unwrap();
            if field == "identity" {
                changed.merge_candidates[0].id = "replacement-candidate".into();
            } else {
                let commit = commit_action_candidate(root.path(), "Replacement candidate");
                changed.merge_candidates[0].candidate_commit = commit.clone();
                changed.attempts[0].tasks[0].output.as_mut().unwrap().commit = commit.clone();
                for task in &mut changed.attempts[0].tasks {
                    if let Some(context) = &mut task.review_context {
                        context.candidate_commit = commit.clone();
                    }
                }
            }
            store.write_work_item(&changed).unwrap();
            assert_eq!(
                project::SourceSnapshot::load(root.path()).unwrap().work[0]
                    .status
                    .action,
                "merge-ready",
                "the changed record must remain otherwise eligible"
            );
            dispatch_action_key(
                &mut app,
                &mut session,
                &mut local,
                if before_confirmation {
                    KeyCode::Enter
                } else {
                    KeyCode::Char('y')
                },
            );
            assert!(
                session.calls.is_empty(),
                "changed {field} must prevent launch (before confirmation: {before_confirmation})"
            );
            assert!(app.pending_action.is_none() && app.confirmed_action.is_none());
            let message = app.action_message.as_ref().unwrap();
            assert!(
                message.contains("Merge Candidate") && message.contains("changed"),
                "{message}"
            );
            if local.pending.is_some() {
                finish_local_refresh(&mut local, &mut app);
            }
        }
    }
}

#[test]
fn project_action_candidate_show_runs_without_confirmation() {
    let (root, mut app) = merge_ready_action_fixture();
    let store = crate::work_model::WorkModelStore::new(root.path());
    let original = store.read_work_item_snapshot("work").unwrap();
    let mut session = ActionSession {
        size: (160, 30),
        calls: vec![],
        outcome: Some(Ok(actions::CommandOutcome::Succeeded)),
    };
    let mut local = LocalRefresh::new(root.path(), &[], root.path());
    assert_eq!(
        app.action_menu.as_ref().unwrap().current().kind,
        actions::ActionKind::CandidateShow
    );
    dispatch_action_key(&mut app, &mut session, &mut local, KeyCode::Enter);
    assert_eq!(session.calls.len(), 1, "read-only show runs immediately");
    assert!(app.pending_action.is_none() && app.confirmed_action.is_none());
    assert_eq!(
        session.calls[0].executable,
        std::env::current_exe().unwrap()
    );
    assert_eq!(
        session.calls[0].args,
        [
            "merge-candidate",
            "show",
            "work",
            &original.merge_candidates[0].id
        ]
    );
    assert_eq!(
        session.calls[0].checkout,
        root.path().canonicalize().unwrap()
    );
    run_dashboard_action(&mut session, &mut app, &mut local, None).unwrap();
    assert_eq!(session.calls.len(), 1);
    assert!(
        app.action_message
            .as_ref()
            .unwrap()
            .contains("Candidate show succeeded")
    );
    assert!(local.pending.is_some());
    finish_local_refresh(&mut local, &mut app);
    assert_eq!(store.read_work_item_snapshot("work").unwrap(), original);
}

fn open_action_for_last_worker(app: &mut App, size: (u16, u16)) {
    app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
    app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
    let source = &mut app.snapshot.sources[0];
    source.work[0].workers = (0..15)
        .map(|n| worker_row(&source.id, &format!("review-{n:02}")))
        .collect();
    app.resize(size.0, size.1);
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    for _ in 0..14 {
        app.handle_key(KeyCode::Down, KeyModifiers::NONE);
    }
    assert!(text(app, size.0, size.1).contains("> review-14"));
    app.handle_key(KeyCode::Char('a'), KeyModifiers::NONE);
}

#[test]
fn project_action_handoff_returns_and_reports_exact_result() {
    for size in [(60, 15), (160, 30)] {
        for (outcome, message) in [
            (Ok(actions::CommandOutcome::Succeeded), "succeeded"),
            (Ok(actions::CommandOutcome::Failed(7)), "failed (exit 7)"),
            (Ok(actions::CommandOutcome::Interrupted), "interrupted"),
            (Err(anyhow::anyhow!("spawn failed")), "spawn failed"),
        ] {
            let (root, mut app) = action_fixture();
            open_action_for_last_worker(&mut app, size);
            let selected = app.selected_worker.clone();
            let before = app.snapshot.sources[0].work[0].status.clone();
            prepare_dashboard_action(&mut app);
            app.handle_key(KeyCode::Char('y'), KeyModifiers::NONE);
            let mut session = ActionSession {
                size,
                calls: vec![],
                outcome: Some(outcome),
            };
            let mut local = LocalRefresh::new(root.path(), &[], root.path());
            run_dashboard_action(&mut session, &mut app, &mut local, None).unwrap();
            let returned = text(&app, size.0, size.1);
            assert!(
                returned.lines().nth(2).unwrap().contains(message),
                "{returned}"
            );
            assert!(returned.contains("> review-14"), "{returned}");
            assert_eq!(app.selected_worker, selected);
            app.handle_key(KeyCode::PageDown, KeyModifiers::NONE);
            assert!(
                text(&app, size.0, size.1)
                    .lines()
                    .nth(2)
                    .unwrap()
                    .contains(message)
            );
            assert_eq!(app.selected_worker, selected);
            assert_eq!(session.calls.len(), 1);
            assert_eq!(
                session.calls[0].executable,
                std::env::current_exe().unwrap()
            );
            assert_eq!(
                session.calls[0].args,
                ["attempt", "run", "work", "attempt-1"]
            );
            assert_eq!(
                session.calls[0].checkout,
                root.path().canonicalize().unwrap()
            );
            assert!(app.action_message.as_ref().unwrap().contains(message));
            assert!(
                local.pending.is_some(),
                "return must request a fresh source read"
            );
            assert_eq!(
                app.snapshot.sources[0].work[0].status, before,
                "never optimistically change Work state"
            );
            run_dashboard_action(&mut session, &mut app, &mut local, None).unwrap();
            assert_eq!(session.calls.len(), 1, "confirmation is consumed once");
            finish_local_refresh(&mut local, &mut app);
            assert!(text(&app, size.0, size.1).contains(message));
            assert!(app.action_menu.is_none());
            app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
            assert!(text(&app, size.0, size.1).contains("Open Work Item"));
        }
    }
}

#[test]
fn project_action_rechecks_selection_and_source_before_launch() {
    for change in ["selection", "source", "eligibility", "remote"] {
        let (root, mut app) = action_fixture();
        open_action_for_last_worker(&mut app, (60, 15));
        prepare_dashboard_action(&mut app);
        assert!(app.pending_action.is_some());
        match change {
            "selection" => app.selected_key = None,
            "source" => app.snapshot.sources[0].path = root.path().join("different"),
            "remote" => app.pending_action.as_mut().unwrap().action.source_id = "ssh:remote".into(),
            _ => {
                let store = crate::work_model::WorkModelStore::new(root.path());
                let mut changed = store.read_work_item_snapshot("work").unwrap();
                changed.authorization = crate::work_model::ExecutionAuthorization::Proposed;
                store.write_work_item(&changed).unwrap();
            }
        }
        app.handle_key(KeyCode::Char('y'), KeyModifiers::NONE);
        let mut session = ActionSession {
            size: (60, 15),
            calls: vec![],
            outcome: None,
        };
        let mut local = LocalRefresh::new(root.path(), &[], root.path());
        run_dashboard_action(&mut session, &mut app, &mut local, None).unwrap();
        let returned = text(&app, 60, 15);
        assert!(
            returned
                .lines()
                .nth(2)
                .unwrap()
                .contains("did not complete"),
            "{returned}"
        );
        assert!(session.calls.is_empty(), "launched after {change} changed");
        assert!(
            app.action_message
                .as_ref()
                .unwrap()
                .contains("did not complete")
        );
        finish_local_refresh(&mut local, &mut app);
    }
    let (root, mut app) = action_fixture();
    prepare_dashboard_action(&mut app);
    let prepared = app.pending_action.take().unwrap();
    let moved = root.path().with_extension("replaced-checkout");
    std::fs::rename(root.path(), &moved).unwrap();
    std::os::unix::fs::symlink(&moved, root.path()).unwrap();
    assert!(
        prepared
            .validate()
            .unwrap_err()
            .to_string()
            .contains("Source checkout changed")
    );
    std::fs::remove_file(root.path()).unwrap();
    std::fs::rename(moved, root.path()).unwrap();
}

#[test]
fn project_action_rechecks_merge_candidate_identity_and_commit() {
    use crate::work_model::{AttemptReviewState, AttemptStatus, TaskOutput, TaskStatus};
    let (root, _) = action_fixture();
    let store = crate::work_model::WorkModelStore::new(root.path());
    let mut item = store.read_work_item_snapshot("work").unwrap();
    let task = &mut item.attempts[0].tasks[0];
    task.status = TaskStatus::Complete;
    task.output = Some(TaskOutput {
        workspace_id: "candidate".into(),
        workspace_path: "../work-9-work-attempt-1".into(),
        source_branch: "candidate".into(),
        base_commit: None,
        commit: "abc123".into(),
        no_change: None,
        learner_canonicalization: None,
    });
    item.attempts[0].status = AttemptStatus::Complete;
    item.attempts[0].review_state = Some(AttemptReviewState::Passed);
    item.create_or_get_merge_candidate("attempt-1").unwrap();
    store.write_work_item(&item).unwrap();
    let source = project::SourceSnapshot::load(root.path()).unwrap();
    let offered = actions::offered(&source, &source.work[0]).remove(0);
    assert_eq!(offered.expected_action, "learner-not-ready");
    let prepared = actions::PreparedAction::prepare(offered.clone()).unwrap();
    assert!(prepared.validate().is_ok());
    for field in ["id", "commit"] {
        let mut changed = store.read_work_item_snapshot("work").unwrap();
        if field == "id" {
            changed.merge_candidates[0].id = "another-candidate".into();
        } else {
            changed.merge_candidates[0].candidate_commit = "def456".into();
            changed.attempts[0].tasks[0].output.as_mut().unwrap().commit = "def456".into();
        }
        store.write_work_item(&changed).unwrap();
        assert!(
            prepared.validate().is_err(),
            "must reject changed {field} after confirmation"
        );
        assert!(
            actions::PreparedAction::prepare(offered.clone()).is_err(),
            "must reject changed {field} before confirmation"
        );
        let mut restored = store.read_work_item_snapshot("work").unwrap();
        restored.merge_candidates = item.merge_candidates.clone();
        restored.attempts = item.attempts.clone();
        store.write_work_item(&restored).unwrap();
    }
}

#[test]
fn project_overview_keeps_workers_visible_with_long_actions_at_minimum_size() {
    let mut source = project_source("/a/long/source/checkout", vec![row("work", "merge-ready")]);
    source.work[0].status.title =
        "A complete descriptive title that spans several lines at the minimum terminal size".into();
    source.work[0].status.merge_candidate = "attempt-1-merge-candidate-with-a-long-id".into();
    source.work[0].workers = vec![
        worker_row(&source.id, "writer"),
        worker_row(&source.id, "tester"),
    ];
    let mut app = project_app(vec![source]);
    app.resize(60, 15);
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    let rendered = text(&app, 60, 15);
    assert!(rendered.contains("Next action"), "{rendered}");
    assert!(rendered.contains("Workers (2)"), "{rendered}");
    assert!(rendered.contains("> writer"), "{rendered}");
    let mut seen = String::new();
    for _ in 0..10 {
        seen.push_str(&text(&app, 60, 15));
        app.handle_key(KeyCode::PageDown, KeyModifiers::NONE);
    }
    assert!(seen.contains("fluent merge-candidate land"));
    assert!(seen.contains("/a/long/source/checkout"));
}

#[test]
fn project_action_event_loop_consumes_confirmation_once() {
    struct Session {
        runner: ActionSession,
        keys: std::collections::VecDeque<KeyCode>,
        frames: Vec<String>,
    }
    impl DashboardSession for Session {
        fn size(&self) -> Result<(u16, u16)> {
            Ok((160, 30))
        }
        fn draw(&mut self, app: &App) -> Result<()> {
            self.frames.push(text(app, 160, 30));
            Ok(())
        }
        fn next_event(&mut self, _: Duration) -> Result<Option<CEvent>> {
            Ok(Some(CEvent::Key(
                self.keys.pop_front().expect("quit was not handled").into(),
            )))
        }
        fn fullscreen(&mut self, _: &mut crate::terminal_view::EmbeddedTerminal) -> Result<()> {
            unreachable!()
        }
        fn is_active(&self) -> bool {
            true
        }
        fn set_mouse_enabled(&mut self, _: bool) -> Result<()> {
            Ok(())
        }
        fn run_action(
            &mut self,
            command: &actions::LocalCommand,
            retained: Option<&mut crate::terminal_view::EmbeddedTerminal>,
        ) -> Result<actions::CommandOutcome> {
            self.runner.run_action(command, retained)
        }
    }
    let (root, mut app) = action_fixture();
    let mut session = Session {
        runner: ActionSession {
            size: (160, 30),
            calls: vec![],
            outcome: Some(Ok(actions::CommandOutcome::Succeeded)),
        },
        keys: [
            KeyCode::Enter,
            KeyCode::Char('y'),
            KeyCode::Char('y'),
            KeyCode::Esc,
            KeyCode::Char('q'),
        ]
        .into(),
        frames: vec![],
    };
    let mut local = LocalRefresh::new(root.path(), &[], root.path());
    let mut remote = crate::dashboard_sources::RemoteSources::new(vec![], Default::default());
    run_project_loop(&mut session, &mut app, &mut local, &mut remote).unwrap();
    assert_eq!(session.runner.calls.len(), 1);
    assert!(
        session
            .frames
            .iter()
            .any(|frame| frame.contains("Confirm local action"))
    );
    assert!(
        session
            .frames
            .iter()
            .any(|frame| frame.contains("Attempt run succeeded"))
    );
    assert!(session.frames.last().unwrap().contains("Open Work Item"));
    assert_eq!(app.page, model::ProjectPage::List);
    if local.pending.is_some() {
        finish_local_refresh(&mut local, &mut app);
    }
}

#[test]
fn project_action_copy_failure_keeps_command_selectable() {
    let (_, mut app) = action_fixture();
    let command = app.action_menu.as_ref().unwrap().current().command_text();
    assert_eq!(
        app.handle_key(KeyCode::Char('c'), KeyModifiers::NONE),
        Effect::CopyAction
    );
    assert_eq!(
        app.complete_copy(Err(anyhow::anyhow!("clipboard unavailable"))),
        Effect::ToggleMouse(false)
    );
    assert!(app.copy_mode);
    let rendered = text(&app, 160, 30);
    assert!(rendered.contains("clipboard unavailable"));
    assert!(rendered.contains(&command));
    assert!(app.pending_action.is_none());
}

#[test]
fn project_overview_treats_section_like_titles_as_display_text() {
    for title in ["Workers (dashboard)", "> an ordinary Work title"] {
        let mut source = project_source("/one", vec![row("work", "task-ready")]);
        source.work[0].status.title = title.into();
        source.work[0].workers = (0..15)
            .map(|n| worker_row("/one", &format!("review-{n:02}")))
            .collect();
        let mut app = project_app(vec![source]);
        app.resize(60, 15);
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        let overview = text(&app, 60, 15);
        assert!(overview.contains(title), "{overview}");
        assert_eq!(app.detail_rows()[0].kind, layout::DetailKind::Text);
        assert!(overview.contains("Next action"), "{overview}");
        assert!(overview.contains("Workers (15)"), "{overview}");
        for _ in 0..14 {
            app.handle_key(KeyCode::Down, KeyModifiers::NONE);
        }
        let overview = text(&app, 60, 15);
        assert!(overview.contains("> review-14"), "{overview}");
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert!(text(&app, 60, 15).contains("Worker: review-14"));
    }
}

#[test]
fn project_overview_footer_keeps_worker_keys_with_actions_and_read_issues() {
    let mut source = project_source("/one", vec![row("work", "task-ready")]);
    source.work[0].workers = vec![worker_row("/one", "writer")];
    let mut hidden = project_source("/hidden", vec![]);
    hidden.stale = Some("read failed".into());
    let mut app = project_app(vec![source, hidden.clone()]);
    app.record_source_read(&hidden);
    app.resize(60, 15);
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    let overview = text(&app, 60, 15);
    let footer = overview.lines().last().unwrap();
    for control in [
        "q quit",
        "Esc Back",
        "Enter Open worker",
        "↑/↓ workers",
        "e Read issues",
    ] {
        assert!(footer.contains(control), "missing {control}: {overview}");
    }
    assert!(overview.contains("a Actions"), "{overview}");
    app.handle_key(KeyCode::Char('e'), KeyModifiers::NONE);
    assert!(text(&app, 60, 15).contains("read failed"));
}

#[test]
fn project_action_feedback_keeps_full_diagnostics_scrollable() {
    let mut source = project_source("/one", vec![row("work", "task-ready")]);
    source.work[0].workers = vec![worker_row("/one", "writer")];
    let mut app = project_app(vec![source]);
    app.resize(60, 15);
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    app.action_message = Some(format!(
        "Attempt run did not complete: {} diagnostic-end",
        "full diagnostic context ".repeat(12)
    ));
    for _ in 0..20 {
        app.handle_key(KeyCode::PageDown, KeyModifiers::NONE);
    }
    let bottom = text(&app, 60, 15);
    assert!(
        bottom
            .lines()
            .nth(2)
            .unwrap()
            .contains("Attempt run did not complete"),
        "{bottom}"
    );
    assert!(bottom.contains("diagnostic-end"), "{bottom}");
    assert!(
        bottom.lines().nth(13).unwrap().contains("Source reads:"),
        "{bottom}"
    );
    let mut snapshot = app.snapshot.clone();
    snapshot.sources[0].work[0].workers[0].elapsed_ms = Some(42000);
    app.refresh(snapshot);
    assert_eq!(text(&app, 60, 15), bottom);
}

#[test]
fn project_overview_preserves_context_scroll_during_elapsed_refresh() {
    for size in [(60, 15), (160, 24)] {
        let mut source = project_source("/one", vec![row("work", "executing")]);
        source.work[0].workers = (0..15)
            .map(|n| worker_row("/one", &format!("review-{n:02}")))
            .collect();
        source.work[0].status.next_action =
            Some("Read the complete guidance before taking another action. ".repeat(12));
        let mut app = project_app(vec![source]);
        app.resize(size.0, size.1);
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        let selected = app.selected_worker.clone();
        for _ in 0..30 {
            app.handle_key(KeyCode::PageDown, KeyModifiers::NONE);
        }
        let context = text(&app, size.0, size.1);
        assert!(context.contains("Technical identifiers"), "{context}");
        assert!(context.contains("Work Item: work"), "{context}");
        assert!(!context.contains("> review-00"));
        let scroll = app.detail_scroll;
        let mut snapshot = app.snapshot.clone();
        snapshot.sources[0].work[0].workers[0].elapsed_ms = Some(42000);
        app.refresh(snapshot);
        assert_eq!(app.selected_worker, selected);
        assert_eq!(app.detail_scroll, scroll);
        assert_eq!(text(&app, size.0, size.1), context);
        // Deliberate worker navigation still reveals the selected row.
        app.handle_key(KeyCode::Down, KeyModifiers::NONE);
        assert!(text(&app, size.0, size.1).contains("> review-01"));
    }
}

#[test]
fn project_detail_keeps_work_identity_after_filter_transition() {
    for worker_page in [false, true] {
        let (root, mut app) = action_fixture();
        app.resize(60, 15);
        let original = app.selected_key.clone();
        prepare_dashboard_action(&mut app);
        app.handle_key(KeyCode::Char('y'), KeyModifiers::NONE);
        let mut session = ActionSession {
            size: (60, 15),
            calls: vec![],
            outcome: Some(Ok(actions::CommandOutcome::Succeeded)),
        };
        let mut local = LocalRefresh::new(root.path(), &[], root.path());
        run_dashboard_action(&mut session, &mut app, &mut local, None).unwrap();
        finish_local_refresh(&mut local, &mut app);
        let source_id = app.snapshot.sources[0].id.clone();
        app.snapshot.sources[0].work[0].workers = vec![worker_row(&source_id, "writer")];
        app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        if worker_page {
            app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
            assert_eq!(app.page, super::model::ProjectPage::Worker);
        }
        let worker = app.selected_worker.clone();
        let mut snapshot = app.snapshot.clone();
        snapshot.sources[0].work[0].category = project::Category::History;
        snapshot.sources[0].work[0].status.action = "merged".into();
        snapshot.sources[0]
            .work
            .extend(project_source(&source_id, vec![row("other", "task-ready")]).work);
        snapshot.sources.push(project_source(
            "/another-source",
            vec![row("work", "task-ready")],
        ));
        app.refresh(snapshot.clone());
        assert_eq!(app.selected_key, original);
        assert_eq!(app.selected_worker, worker);
        assert!(text(&app, 60, 15).contains("succeeded"));
        assert!(
            text(&app, 60, 15)
                .lines()
                .next()
                .unwrap()
                .contains("History")
        );
        // Deleting the exact item must never retarget its result to another source.
        snapshot.sources[0].work.remove(0);
        app.refresh(snapshot.clone());
        snapshot.sources[0].work[0].updated_at = Some("2026-11-07T10:00:00Z".into());
        app.refresh(snapshot);
        assert!(app.selected_key.is_none());
        assert!(
            app.detail_values()
                .join("\n")
                .contains(&root.path().canonicalize().unwrap().display().to_string())
        );
        if worker_page {
            app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        }
        app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(app.selected_id(), Some("other"));
        let list = text(&app, 60, 15);
        assert!(
            list.lines().any(|line| line.starts_with("> other")),
            "{list}"
        );
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert!(!text(&app, 60, 15).contains("succeeded"));
        assert!(!app.detail_values().join("\n").contains("succeeded"));
    }
}

#[test]
fn project_list_paging_moves_visible_selection_and_survives_refresh() {
    for (width, height) in [(60, 15), (160, 24)] {
        let mut app = project_app(
            (0..4)
                .map(|source| {
                    project_source(
                        &format!("/source-{source}"),
                        (0..12)
                            .map(|n| row(&format!("work-{source}-{n:02}"), "task-ready"))
                            .collect(),
                    )
                })
                .collect(),
        );
        app.resize(width, height);
        for direction in [
            KeyCode::PageDown,
            KeyCode::PageDown,
            KeyCode::PageDown,
            KeyCode::PageUp,
            KeyCode::PageUp,
        ] {
            let before = app.selected_key.clone();
            app.handle_key(direction, KeyModifiers::NONE);
            assert_ne!(app.selected_key, before);
            let selected = app.selected_key.clone().unwrap();
            let rendered = text(&app, width, height);
            assert!(
                rendered
                    .lines()
                    .any(|line| line.starts_with(&format!("> {} title", selected.work_item_id))),
                "{rendered}"
            );
            let scroll = app.list_scroll;
            let mut snapshot = app.snapshot.clone();
            snapshot.sources[0].work[0].updated_at = Some(format!("refresh-{scroll}"));
            app.refresh(snapshot);
            assert_eq!(app.selected_key.as_ref(), Some(&selected));
            assert_eq!(app.list_scroll, scroll);
            app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
            assert!(
                text(&app, width, height).contains(&format!("{} title", selected.work_item_id))
            );
            app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
            assert_eq!(app.selected_key.as_ref(), Some(&selected));
            assert_eq!(app.list_scroll, scroll);
        }
        // Page boundaries and source headers still leave a visible selectable row.
        for key in [KeyCode::PageDown, KeyCode::PageUp] {
            for _ in 0..20 {
                app.handle_key(key, KeyModifiers::NONE);
            }
            let rendered = text(&app, width, height);
            assert!(
                rendered.lines().any(
                    |line| line.starts_with(&format!("> {} title", app.selected_id().unwrap()))
                ),
                "{rendered}"
            );
        }
    }
}
