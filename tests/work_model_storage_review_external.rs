use factory::work_model::{
    Attempt, AttemptReviewState, AttemptStatus, Task, TaskArtifactArea, TaskKind, WorkItem,
    WorkModelError, WorkModelStorageError, WorkModelStore, WorkspaceAccess, WorkspaceRef,
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
        attempts: vec![Attempt {
            id: "attempt-review".to_string(),
            work_item_id: "work-review".to_string(),
            status: AttemptStatus::Complete,
            tasks: vec![Task {
                id: "write-review".to_string(),
                kind: TaskKind::Write,
                role: "author".to_string(),
                work_item_id: "work-review".to_string(),
                attempt_id: Some("attempt-review".to_string()),
                workspace_access: WorkspaceAccess {
                    reads: vec![workspace("main")],
                    writes: vec![workspace("candidate")],
                },
                artifact_area: Some(TaskArtifactArea {
                    path: ".factory/work/artifacts/write-review".to_string(),
                }),
            }],
            review_state: Some(AttemptReviewState::Passed),
            artifacts: Vec::new(),
        }],
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
  "title": "Review durable storage",
  "attempts": [
    {
      "id": "attempt-review",
      "work_item_id": "work-review",
      "status": "complete",
      "tasks": [
        {
          "id": "write-review",
          "kind": "write",
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
          }
        }
      ],
      "review_state": "passed",
      "artifacts": []
    }
  ]
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
    let invalid_model = items.join("work-review.json");
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
  "title": "Review durable storage",
  "attempts": [
    {
      "id": "attempt-review",
      "work_item_id": "work-review",
      "status": "complete",
      "tasks": [
        {
          "id": "write-review",
          "kind": "write",
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
          }
        }
      ],
      "review_state": "passed",
      "artifacts": []
    }
  ]
}
"#
    );
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
