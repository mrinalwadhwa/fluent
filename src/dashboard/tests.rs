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
    assert!(
        snapshot
            .errors
            .iter()
            .any(|d| d.contains("host-owned recovery is pending"))
    );
}

#[test]
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
    let mut app = app(vec![row("first", "planned"), row("selected", "planned")]);
    app.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
    app.refresh(snapshot::DashboardSnapshot::from_status(
        crate::work_status::WorkStatus {
            rows: vec![row("selected", "executing"), row("first", "planned")],
            errors: vec![],
        },
    ));
    assert_eq!(app.selected_id(), Some("selected"));
    app.handle_key(KeyCode::Char('a'), KeyModifiers::NONE);
    assert_eq!(app.selected_id(), Some("selected"));
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
    let mut app = app(vec![row("work", "planned")]);
    let before = app.snapshot.rows[0].status.clone();
    for key in ['j', 'a', '?', 'r', 'c'] {
        app.handle_key(KeyCode::Char(key), KeyModifiers::NONE);
    }
    assert_eq!(app.snapshot.rows[0].status, before);
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
    app.filter = project::Filter::Category(C::NeedsYou);
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
        project::SourceSnapshot::load(proposed_root.path()).unwrap().work[0].category,
        C::Planned
    );
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
    let source = project::SourceSnapshot::load(root.path()).unwrap();
    assert_eq!(source.work[0].workers.len(), 2);
    assert_eq!(source.work[0].workers[0].role, "architecture");
    assert_eq!(source.work[0].workers[1].role, "tests");
    assert!(source.work[0].workers[0].terminal.is_none());
    let mut app = project_app(vec![source]);
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.project_keys().len(), 3);
    assert!(text(&app, 100, 24).contains("Current 1"));
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
        terminal: None,
        unavailable_reason: "headless execution".into(),
    });
    let mut app = project_app(vec![source]);
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    app.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
    let details = layout::project_detail_values(&app, app.selected_key.as_ref().unwrap());
    assert!(details.contains(&"Provider: Codex".into()));
    assert!(details.contains(&"Model: unavailable".into()));
    assert!(details.contains(&"Elapsed: unavailable".into()));
    assert!(
        details
            .iter()
            .any(|value| value.contains("headless execution"))
    );
    let ready = project_app(vec![project_source(
        "/one",
        vec![row("work", "task-ready")],
    )]);
    assert!(
        layout::project_detail_values(&ready, ready.selected_key.as_ref().unwrap())
            .contains(&"Next action: fluent attempt run work".into())
    );
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
            terminal: None,
            unavailable_reason: "unavailable".into(),
        });
    }
    let mut app = project_app(vec![first.clone(), second.clone()]);
    app.select_key(second.work[0].key.clone());
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    app.select_key(second.work[0].workers[0].key.clone());
    app.refresh(snapshot::DashboardSnapshot {
        rows: vec![],
        errors: vec![],
        sources: vec![second, first],
        project_view: true,
    });
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
    let old = project::SourceSnapshot::load(&one).unwrap();
    assert_eq!(old.work.len(), 1, "{:?}", old.diagnostics);
    let items = one.join(".fluent/work/items");
    std::fs::remove_dir_all(&items).unwrap();
    std::fs::write(&items, "broken").unwrap();
    let refreshed = project::refresh_sources(&[old], &[one.clone(), two]);
    assert_eq!(refreshed.len(), 2);
    assert!(refreshed[0].stale.is_some());
    assert_eq!(refreshed[0].work[0].status.title, "Retain me");
    assert!(refreshed[1].stale.is_none());
}

#[test]
fn project_layout_keeps_selected_content_visible() {
    let mut app = project_app(vec![project_source(
        "/one",
        (0..30)
            .map(|n| row(&format!("work-{n}"), "planned"))
            .collect(),
    )]);
    for (width, height) in [(59, 14), (60, 15), (99, 18), (100, 18), (140, 24)] {
        app.resize(width, height);
        for _ in 0..29 {
            app.handle_key(KeyCode::Char('j'), KeyModifiers::NONE);
        }
        let rendered = text(&app, width, height);
        if width < 60 {
            assert!(rendered.contains("Resize terminal"));
        } else {
            assert!(rendered.contains("work-29"));
        }
    }
    let mut compact = project_app(vec![project_source(
        "/one",
        vec![row("長い", "needs-user")],
    )]);
    compact.snapshot.sources[0].work[0].status.title =
        "漢字 e\u{301} and a very long Work title that wraps safely".into();
    compact.resize(60, 15);
    compact.handle_key(KeyCode::Tab, KeyModifiers::NONE);
    let rendered = text(&compact, 60, 15);
    assert!(rendered.contains("Selected details"));
    assert!(rendered.contains('漢') && rendered.contains('字'));
    compact.resize(100, 18);
    let mut terminal = Terminal::new(TestBackend::new(100, 18)).unwrap();
    terminal
        .draw(|frame| render::draw(frame, &compact))
        .unwrap();
    for y in 6..16 {
        assert_eq!(terminal.backend().buffer()[(42, y)].symbol(), "│");
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
