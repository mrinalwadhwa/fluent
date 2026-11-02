use super::*;
use crate::work_status::WorkItemStatus;
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

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
    assert!(text(&app, 140, 24).contains("> ▸ Updated local"));
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
    assert!(text(&app, 140, 24).contains("> ▸ selected title"));
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
    });
    let mut terminal = Terminal::new(TestBackend::new(140, 30)).unwrap();
    terminal.draw(|frame| render::draw(frame, &app)).unwrap();
    let buffer = terminal.backend().buffer();
    let rendered = buffer_text(buffer);
    assert!(rendered.contains("same-name"), "{rendered}");
    let content = layout::terminal_content((140, 30), &app.terminal.as_ref().unwrap().identity);
    assert_eq!(buffer[(content.x + 7, content.y)].symbol(), "界");
    assert_eq!(buffer[(content.x + 10, content.y)].symbol(), "a");
    assert_eq!(buffer[(content.x - 1, content.y)].symbol(), "│");
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
        let header: String = (4..area.y)
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
    let selected = app.selected_key.clone();
    assert_eq!(
        app.handle_key(KeyCode::Char('f'), KeyModifiers::NONE),
        Effect::FullscreenTerminal
    );
    app.resize(150, 40);
    let mut refreshed = app.snapshot.clone();
    refreshed.errors.push("attachment failed".into());
    refreshed.sources[0].work.clear();
    app.refresh(refreshed);
    assert_eq!(app.selected_key, selected);
    assert_eq!(app.size, (150, 40));
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
    assert_eq!(app.project_keys().len(), 3);
    let rendered = text(&app, 140, 24);
    assert!(rendered.contains("All 1"));
    assert!(rendered.contains("architecture"));
    assert!(rendered.contains("tests"));
    assert!(rendered.contains("architecture") && rendered.contains("· session"));
    assert!(rendered.contains("tests") && rendered.contains("no session"));
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
        layout::project_detail_values(&app.snapshot.sources, app.selected_key.as_ref().unwrap());
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
    assert!(rendered.contains("· session"));
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
    let rendered = text(&app, 140, 30);
    assert!(rendered.lines().any(|line| line.contains("architecture")
        && line.contains("session")
        && line.contains("complete")));
    assert!(rendered.lines().any(|line| line.contains("tests")
        && line.contains("no session")
        && line.contains("planned")));
    app.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
    let details = text(&app, 140, 30);
    assert!(details.contains("Provider: fixture"));
    assert!(details.contains("Model: review-model"));
    assert!(details.contains("Effort: high"));
    assert!(details.contains("Elapsed: 12s"));
    assert!(details.contains("Agent terminal: Observe available"));
    app.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
    let unavailable = text(&app, 140, 30);
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
        app.selected_key
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
    app.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
    assert_eq!(
        app.selected_key.as_ref().unwrap().task_id.as_deref(),
        Some("write")
    );
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
        app.selected_key
            .as_ref()
            .unwrap()
            .execution_generation
            .as_deref(),
        Some("generation-3")
    );
    assert!(text(&app, 80, 20).contains("Writer"));
    let mut removed = app.snapshot.clone();
    removed.sources[0].work[0].workers.clear();
    removed.sources[1].work[0].status.title = "Neighbor".into();
    app.refresh(removed);
    assert_eq!(
        app.selected_key,
        Some(project::RowKey::work("/one", "same"))
    );
    assert!(text(&app, 80, 20).contains("> ▸ Neighbor"));
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
    assert!(rendered.contains("STALE"));
    assert_eq!(rendered.matches("STALE").count(), 1);
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
            assert!(rendered.contains("> ▸ work-29"));
            assert!(rendered.contains('漢') && rendered.contains('字'));
            assert!(rendered.contains("e\u{301}"));
            assert!(rendered.contains("q quit"));
            assert!(rendered.contains("? help"));
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| render::draw(frame, &app)).unwrap();
            if width >= 100 {
                let boundary =
                    layout::project_areas(ratatui::layout::Rect::new(0, 0, width, height))
                        .divider
                        .x;
                for y in 3..height - 1 {
                    assert_eq!(terminal.backend().buffer()[(boundary, y)].symbol(), "│");
                }
            }
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
    assert!(rendered.contains("Details"));
    assert!(rendered.contains("Work Item: 長"));
    compact.resize(100, 18);
    let mut terminal = Terminal::new(TestBackend::new(100, 18)).unwrap();
    terminal
        .draw(|frame| render::draw(frame, &compact))
        .unwrap();
    for y in 3..17 {
        assert_eq!(terminal.backend().buffer()[(65, y)].symbol(), "│");
    }
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
            assert!(lines[0].starts_with("Fluent work"), "{rendered}");
            assert!(
                lines[1].contains("All 1  Needs you 0  Running 1  Ready 0  Waiting 0  History 0"),
                "{rendered}"
            );
            assert!(lines[2].chars().all(|c| c == '─'), "{rendered}");
            for line in &lines[3..height as usize - 1] {
                assert_eq!(line.matches('│').count(), 1, "{rendered}");
                assert!(!line.contains(['┌', '┐', '└', '┘', '─']), "{rendered}");
            }
            let divider = lines[3].chars().position(|c| c == '│').unwrap();
            assert!(divider >= width as usize * 3 / 5);
            for y in 3..height - 1 {
                assert_eq!(buffer[(divider as u16, y)].symbol(), "│");
            }
            assert!(lines[3].contains("Tasks") && lines[3].contains("Status"));
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
    assert!(lines[4].contains("STALE"), "{rendered}");
    assert!(
        lines[5].contains('漢')
            && lines[5].contains('字')
            && lines[5].contains("e\u{301}")
            && lines[5].contains("Running"),
        "{rendered}"
    );
    assert!(lines[6].contains("Ready to land"), "{rendered}");
    assert!(lines[7].contains("Planned"), "{rendered}");
    assert_eq!(buffer[(0, 5)].symbol(), ">");
    assert_ne!(buffer[(0, 5)].bg, buffer[(0, 6)].bg);
    let status_x = lines[3].find("Status").unwrap() as u16;
    assert_eq!(buffer[(status_x, 5)].symbol(), "R");
    assert_eq!(buffer[(status_x, 6)].symbol(), "R");
    assert!(lines[3].contains("Updated"));
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
    assert_eq!(app.project_keys().len(), 8);
    let cases = [
        (Filter::NeedsYou, vec!["attention"]),
        (Filter::Running, vec!["run"]),
        (Filter::Ready, vec!["ready", "land"]),
        (Filter::Waiting, vec!["queue", "plan"]),
        (Filter::History, vec!["past"]),
        (
            Filter::All,
            vec![
                "attention",
                "run",
                "ready",
                "land",
                "queue",
                "plan",
                "past",
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
                .nth(1)
                .unwrap()
                .contains("All 8  Needs you 1  Running 1  Ready 2  Waiting 2  History 1")
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
    assert_eq!(app.selected_key, selected);
    app.handle_key(KeyCode::Tab, KeyModifiers::SHIFT);
    assert_eq!(app.filter, Filter::History);
    app.handle_key(KeyCode::Char('f'), KeyModifiers::NONE);
    assert_eq!(app.filter, Filter::All);
    app.handle_key(KeyCode::Char('a'), KeyModifiers::NONE);
    assert_eq!(app.filter, Filter::History);
    app.handle_key(KeyCode::Char('a'), KeyModifiers::NONE);
    assert_eq!(app.filter, Filter::All);
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
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    let rendered = text(&app, 120, 28);
    assert!(rendered.contains("Updated UTC"), "{rendered}");
    assert!(rendered.contains(&updated), "{rendered}");
    assert_eq!(rendered.matches(&updated).count(), 2, "{rendered}");
}

#[test]
fn project_details_prioritize_action_without_redundant_row_copy() {
    let mut status = row("WI-42", "needs-user");
    status.title = "Do not repeat this selected title".into();
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
    for worker in [false, true] {
        if worker {
            app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
            app.handle_key(KeyCode::Down, KeyModifiers::NONE);
        }
        let values = layout::project_detail_values(
            &app.snapshot.sources,
            app.selected_key.as_ref().unwrap(),
        );
        let joined = values.join("\n");
        assert!(
            !joined.contains("Do not repeat this selected title"),
            "{joined}"
        );
        assert!(!joined.contains("Status: Needs you"), "{joined}");
        assert!(joined.contains("Work Item: WI-42"));
        assert!(joined.contains("Next action: fluent work-item show WI-42"));
        assert!(joined.find("Next action:").unwrap() < joined.find("Source:").unwrap());
        if worker {
            for field in [
                "Task: write-1",
                "Invocation: generation-1",
                "State: executing",
                "headless execution",
            ] {
                assert!(joined.contains(field), "{joined}");
            }
        } else {
            assert!(joined.contains("Host proof is missing"));
        }
        let rendered = text(&app, 120, 28);
        let detail = rendered
            .lines()
            .skip(3)
            .take(24)
            .map(|line| line.split('│').nth(1).unwrap_or(""))
            .collect::<String>();
        assert!(detail.contains("Next action:"), "{rendered}");
        assert!(!detail.contains("Do not repeat"), "{rendered}");
    }
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
fn arrow_drill_requires_selected_available_worker() {
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
    assert_eq!(app.project_keys().len(), 3);
    app.handle_key(KeyCode::Down, KeyModifiers::NONE);
    let selected = app.selected_key.clone();
    assert_eq!(
        app.handle_key(KeyCode::Right, KeyModifiers::NONE),
        Effect::OpenTerminal
    );
    assert_eq!(app.selected_key, selected);
    app.pane = app::Pane::List;
    app.handle_key(KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(
        app.handle_key(KeyCode::Right, KeyModifiers::NONE),
        Effect::None
    );
    assert!(text(&app, 120, 28).contains("headless execution"));
    assert!(app.terminal.is_none());
    app.pane = app::Pane::List;
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
    app.pane = app::Pane::List;
    app.handle_key(KeyCode::Up, KeyModifiers::NONE);
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.project_keys().len(), 1);
}

#[test]
#[cfg(feature = "test-support")]
fn arrow_drill_observes_without_retargeting_on_selection() {
    with_terminal_client(
        "arrow_drill_observes_without_retargeting_on_selection",
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
            app.handle_key(KeyCode::Down, KeyModifiers::NONE);
            handle_terminal_key(
                &mut app,
                &mut embedded,
                KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
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
            app.pane = app::Pane::List;
            handle_terminal_key(
                &mut app,
                &mut embedded,
                KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
                |_| Ok(()),
            );
            assert!(
                embedded.is_none(),
                "moving selection must wait for Right before opening another worker"
            );
            assert!(app.terminal.is_none());
            handle_terminal_key(
                &mut app,
                &mut embedded,
                KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
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
        app.filter = project::Filter::All;
        assert!(!text(&app, width, 24).contains("Work Item: work"));
        app.handle_key(KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.filter, project::Filter::NeedsYou);
        assert_eq!(app.pane, app::Pane::List);
        app.handle_key(KeyCode::Right, KeyModifiers::NONE);
        let rendered = text(&app, width, 24);
        assert!(rendered.contains("Work Item: work"), "{rendered}");
        assert!(rendered.contains("←/Esc list"), "{rendered}");
        app.handle_key(KeyCode::BackTab, KeyModifiers::SHIFT);
        assert_eq!(app.filter, project::Filter::All);
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
            app.set_terminal(display);
            let rendered = text(&app, width, height);
            assert!(rendered.contains(state), "{width}x{height}: {rendered}");
            assert!(rendered.contains("←/Esc"), "{rendered}");
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
            KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
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
    app.handle_key(KeyCode::BackTab, KeyModifiers::SHIFT);
    assert!(
        text(&app, 60, 15)
            .lines()
            .nth(1)
            .unwrap()
            .contains("History 120")
    );
    app.snapshot.sources[0].diagnostics =
        vec!["a long diagnostic with many words ".repeat(70) + "END-DIAGNOSTIC"];
    app.handle_key(KeyCode::Right, KeyModifiers::NONE);
    for _ in 0..300 {
        app.handle_key(KeyCode::Down, KeyModifiers::NONE);
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
            assert!(self.frames[0].contains("Loading"));
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
    assert!(session.frames.iter().any(|frame| frame.contains("? close")));
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
    assert!(text(&app, 140, 24).contains("> ▸ New remote state"));
    assert!(text(&app, 140, 24).contains("local title"));
}

#[test]
fn refresh_health_distinguishes_loading_success_and_error() {
    let root = tempfile::tempdir().unwrap();
    let id = root
        .path()
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let mut app = loading_project_app();
    let mut local = LocalRefresh::new(root.path(), &[], root.path());
    assert!(text(&app, 160, 30).contains("Loading local Work"));
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
    assert!(text(&app, 160, 30).contains("Read failed"));
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
    assert!(text(&app, 160, 30).contains("Last read UTC"));
    assert!(text(&app, 160, 30).contains("Updated UTC"));
    // Equal source values still record a new successful read and request a draw.
    app.take_dirty();
    local.request(&mut app, false);
    assert!(text(&app, 160, 30).contains("Refreshing"));
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
    app.handle_key(KeyCode::Char('e'), KeyModifiers::NONE);
    assert!(text(&app, 160, 30).contains("STALE"));
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
    assert!(text(&app, 160, 30).contains("Read failed"));
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
