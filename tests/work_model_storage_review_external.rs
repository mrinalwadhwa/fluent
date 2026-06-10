use factory::work_model::{
    Attempt, AttemptKind, AttemptReviewState, AttemptStatus, Task, TaskArtifactArea, TaskKind,
    TaskOutput, TaskStatus, WorkItem, WorkModelError, WorkModelStorageError, WorkModelStore,
    WorkspaceAccess, WorkspaceRef,
};
use std::fs;

fn workspace(id: &str) -> WorkspaceRef {
    WorkspaceRef {
        id: id.to_string(),
        path: format!("../workspaces/{id}"),
    }
}

fn documented_work_item() -> WorkItem {
    WorkItem {
        id: "work-review".to_string(),
        title: "Review durable storage".to_string(),
        planning_context: None,
        instructions: None,
        abandonment: None,
        attempts: vec![Attempt {
            id: "attempt-review".to_string(),
            work_item_id: "work-review".to_string(),
            kind: AttemptKind::Write,
            status: AttemptStatus::Complete,
            tasks: vec![Task {
                id: "write-review".to_string(),
                kind: TaskKind::Write,
                status: TaskStatus::Complete,
                role: "author".to_string(),
                instructions: None,
                work_item_id: "work-review".to_string(),
                attempt_id: Some("attempt-review".to_string()),
                workspace_access: WorkspaceAccess {
                    reads: vec![workspace("main")],
                    writes: vec![workspace("candidate")],
                },
                artifact_area: Some(TaskArtifactArea {
                    path: ".factory/work/artifacts/write-review".to_string(),
                }),
                review_context: None,
                input_artifacts: Vec::new(),
                output: Some(TaskOutput {
                    workspace_id: "candidate".to_string(),
                    workspace_path: "../workspaces/candidate".to_string(),
                    source_branch: "main".to_string(),
                    commit: "abc123".to_string(),
                }),
            }],
            review_state: Some(AttemptReviewState::Passed),
            artifacts: Vec::new(),
        }],
        merge_candidates: Vec::new(),
    }
}

#[test]
fn reviewer_storage_reads_documented_layout() {
    let temp = tempfile::tempdir().unwrap();
    let items = temp.path().join(".factory/work/items");
    fs::create_dir_all(&items).unwrap();
    fs::write(
        items.join("work-review.json"),
        r#"{
  "id": "work-review",
  "title": "Review durable storage"
}
"#,
    )
    .unwrap();
    fs::create_dir_all(temp.path().join(".factory/work/attempts/work-review")).unwrap();
    fs::write(
        temp.path()
            .join(".factory/work/attempts/work-review/attempt-review.json"),
        r#"{
  "id": "attempt-review",
  "work_item_id": "work-review",
  "order": 0,
  "status": "complete",
  "review_state": "passed",
  "artifacts": []
}
"#,
    )
    .unwrap();
    fs::create_dir_all(
        temp.path()
            .join(".factory/work/tasks/work-review/attempt-review"),
    )
    .unwrap();
    fs::write(
        temp.path()
            .join(".factory/work/tasks/work-review/attempt-review/write-review.json"),
        r#"{
  "order": 0,
  "id": "write-review",
  "kind": "write",
  "status": "complete",
  "role": "author",
  "work_item_id": "work-review",
  "attempt_id": "attempt-review",
  "workspace_access": {
    "reads": [
      {
        "id": "main",
        "path": "../workspaces/main"
      }
    ],
    "writes": [
      {
        "id": "candidate",
        "path": "../workspaces/candidate"
      }
    ]
  },
  "artifact_area": {
    "path": ".factory/work/artifacts/write-review"
  },
  "output": {
    "workspace_id": "candidate",
    "workspace_path": "../workspaces/candidate",
    "source_branch": "main",
    "commit": "abc123"
  }
}
"#,
    )
    .unwrap();

    let store = WorkModelStore::new(temp.path());

    assert_eq!(
        store.read_work_item("work-review").unwrap(),
        documented_work_item()
    );
    assert_eq!(
        store.list_work_items().unwrap(),
        vec![documented_work_item()]
    );
}

#[test]
fn reviewer_storage_reports_invalid_files() {
    let temp = tempfile::tempdir().unwrap();
    let items = temp.path().join(".factory/work/items");
    fs::create_dir_all(&items).unwrap();
    let invalid_json = items.join("broken-json.json");
    fs::write(&invalid_json, "{").unwrap();

    match WorkModelStore::new(temp.path())
        .read_work_item("broken-json")
        .unwrap_err()
    {
        WorkModelStorageError::ParseFile { path, .. } => assert_eq!(path, invalid_json),
        other => panic!("unexpected invalid JSON error: {other}"),
    }

    let store = WorkModelStore::new(temp.path());
    store.write_work_item(&documented_work_item()).unwrap();
    let invalid_model = temp
        .path()
        .join(".factory/work/tasks/work-review/attempt-review/write-review.json");
    let content = fs::read_to_string(&invalid_model)
        .unwrap()
        .replace(r#""kind": "write""#, r#""kind": "review""#);
    fs::write(&invalid_model, content).unwrap();

    match store.read_work_item("work-review").unwrap_err() {
        WorkModelStorageError::InvalidModel { path, source } => {
            assert_eq!(path, invalid_model);
            assert_eq!(
                source,
                WorkModelError::ReviewTaskWritesWorkspace {
                    task_id: "write-review".to_string(),
                }
            );
        }
        other => panic!("unexpected invalid model error: {other}"),
    }
}

#[test]
fn reviewer_storage_writes_documented_deterministic_json() {
    let temp = tempfile::tempdir().unwrap();
    let store = WorkModelStore::new(temp.path());

    store.write_work_item(&documented_work_item()).unwrap();

    let path = temp.path().join(".factory/work/items/work-review.json");
    assert_eq!(
        fs::read_to_string(path).unwrap(),
        r#"{
  "id": "work-review",
  "title": "Review durable storage"
}
"#
    );
    let attempt = fs::read_to_string(
        temp.path()
            .join(".factory/work/attempts/work-review/attempt-review.json"),
    )
    .unwrap();
    assert!(attempt.contains(r#""id": "attempt-review""#));
    assert!(attempt.contains(r#""review_state": "passed""#));
    assert!(!attempt.contains(r#""tasks""#));

    let task = fs::read_to_string(
        temp.path()
            .join(".factory/work/tasks/work-review/attempt-review/write-review.json"),
    )
    .unwrap();
    assert!(task.contains(r#""id": "write-review""#));
    assert!(task.contains(r#""kind": "write""#));
    assert!(task.contains(r#""commit": "abc123""#));
}

#[test]
fn reviewer_storage_keeps_run_artifacts_separate() {
    let temp = tempfile::tempdir().unwrap();
    let run_dir = temp
        .path()
        .join(".factory/runs/run-legacy/sessions/session-1");
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(
        temp.path().join(".factory/runs/run-legacy/status"),
        "complete",
    )
    .unwrap();
    fs::write(run_dir.join("transcript.jsonl"), "{}\n").unwrap();

    let store = WorkModelStore::new(temp.path());
    store.write_work_item(&documented_work_item()).unwrap();

    assert_eq!(
        fs::read_to_string(temp.path().join(".factory/runs/run-legacy/status")).unwrap(),
        "complete"
    );
    assert_eq!(
        fs::read_to_string(run_dir.join("transcript.jsonl")).unwrap(),
        "{}\n"
    );
    assert!(
        temp.path()
            .join(".factory/work/items/work-review.json")
            .exists()
    );
}
