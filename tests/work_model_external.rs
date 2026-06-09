use factory::work_model::{
    Attempt, AttemptKind, AttemptReviewState, AttemptStatus, MergeCandidate,
    MergeCandidateMergeState, MergeCandidateReviewState, ReviewContext, Task, TaskArtifactArea,
    TaskKind, TaskOutput, TaskStatus, WorkItem, WorkModelError, WorkModelStorageError,
    WorkModelStore, WorkspaceAccess, WorkspaceRef, from_json,
};
use std::fs;

fn workspace(id: &str) -> WorkspaceRef {
    WorkspaceRef {
        id: id.to_string(),
        path: format!("../workspaces/{id}"),
    }
}

fn task(kind: TaskKind) -> Task {
    Task {
        id: "write-code".to_string(),
        kind,
        status: TaskStatus::Complete,
        role: "author".to_string(),
        instructions: None,
        work_item_id: "work-1".to_string(),
        attempt_id: Some("attempt-1".to_string()),
        workspace_access: WorkspaceAccess {
            reads: vec![workspace("main")],
            writes: vec![workspace("candidate")],
        },
        artifact_area: Some(TaskArtifactArea {
            path: ".factory/work/artifacts/write-code".to_string(),
        }),
        review_context: None,
        input_artifacts: Vec::new(),
        output: Some(TaskOutput {
            workspace_id: "candidate".to_string(),
            workspace_path: "../workspaces/candidate".to_string(),
            source_branch: "main".to_string(),
            commit: "abc123".to_string(),
        }),
    }
}

fn work_item() -> WorkItem {
    WorkItem {
        id: "work-1".to_string(),
        title: "Add durable model storage".to_string(),
        planning_context: None,
        instructions: None,
        attempts: vec![Attempt {
            id: "attempt-1".to_string(),
            work_item_id: "work-1".to_string(),
            kind: AttemptKind::Write,
            status: AttemptStatus::Complete,
            tasks: vec![task(TaskKind::Write)],
            review_state: Some(AttemptReviewState::Passed),
            artifacts: Vec::new(),
        }],
        merge_candidates: Vec::new(),
    }
}

fn merge_candidate() -> MergeCandidate {
    MergeCandidate {
        id: "attempt-1-merge-candidate".to_string(),
        attempt_id: "attempt-1".to_string(),
        source_workspace: workspace("candidate"),
        target_workspace: workspace("main"),
        source_branch: "main".to_string(),
        target_branch: "main".to_string(),
        candidate_commit: "abc123".to_string(),
        review_state: MergeCandidateReviewState::Pending,
        merge_state: MergeCandidateMergeState::default(),
    }
}

#[test]
fn documented_task_kinds_parse_from_json() {
    let content = include_str!("fixtures/core-work-model/task-kinds.json");
    let kinds: Vec<TaskKind> = from_json(content).unwrap();

    assert_eq!(
        kinds,
        vec![
            TaskKind::Write,
            TaskKind::Review,
            TaskKind::Merge,
            TaskKind::Report,
            TaskKind::Learn,
            TaskKind::Probe,
        ]
    );
}

#[test]
fn documented_review_task_reads_candidate_workspace() {
    let content = include_str!("fixtures/core-work-model/task-review-read-only.json");
    let task: Task = from_json(content).unwrap();

    task.validate().unwrap();
    assert_eq!(task.kind, TaskKind::Review);
    assert_eq!(task.workspace_access.reads.len(), 2);
    assert!(task.workspace_access.writes.is_empty());
    assert!(task.artifact_area.is_some());
    let review_context = task.review_context.unwrap();
    assert_eq!(review_context.candidate_workspace_id, "candidate");
    assert_eq!(review_context.candidate_workspace_path, "../run-work-1");
}

#[test]
fn documented_task_definition_rejects_multiple_write_workspaces() {
    let content = include_str!("fixtures/core-work-model/task-write-two-workspaces.json");
    let task: Task = from_json(content).unwrap();

    assert_eq!(
        task.validate().unwrap_err(),
        WorkModelError::MultipleWriteWorkspaces { count: 2 }
    );
}

#[test]
fn documented_review_task_rejects_workspace_writes() {
    let content = include_str!("fixtures/core-work-model/task-review-writes-workspace.json");
    let task: Task = from_json(content).unwrap();

    assert_eq!(
        task.validate().unwrap_err(),
        WorkModelError::ReviewTaskWritesWorkspace {
            task_id: "review-architecture".to_string(),
        }
    );
}

#[test]
fn documented_review_task_rejects_missing_context() {
    let content = include_str!("fixtures/core-work-model/task-review-read-only.json");
    let mut value: serde_json::Value = serde_json::from_str(content).unwrap();
    value.as_object_mut().unwrap().remove("review_context");
    let task: Task = serde_json::from_value(value).unwrap();

    assert_eq!(
        task.validate().unwrap_err(),
        WorkModelError::ReviewTaskMissingContext {
            task_id: "review-architecture".to_string(),
        }
    );
}

#[test]
fn documented_review_task_rejects_context_candidate_mismatch() {
    let content = include_str!("fixtures/core-work-model/task-review-read-only.json");
    let mut value: serde_json::Value = serde_json::from_str(content).unwrap();
    value["review_context"]["candidate_workspace_path"] =
        serde_json::Value::String("../other-workspace".to_string());
    let task: Task = serde_json::from_value(value).unwrap();

    assert_eq!(
        task.validate().unwrap_err(),
        WorkModelError::ReviewTaskContextCandidateNotReadable {
            task_id: "review-architecture".to_string(),
        }
    );
}

#[test]
fn work_model_store_writes_and_lists_documented_layout() {
    let temp = tempfile::tempdir().unwrap();
    let store = WorkModelStore::new(temp.path());
    let work_item = work_item();

    store.write_work_item(&work_item).unwrap();

    assert!(temp.path().join(".factory/work/items/work-1.json").exists());
    assert!(
        temp.path()
            .join(".factory/work/attempts/work-1/attempt-1.json")
            .exists()
    );
    assert!(
        temp.path()
            .join(".factory/work/tasks/work-1/attempt-1/write-code.json")
            .exists()
    );
    assert!(!temp.path().join(".factory/runs").exists());
    assert_eq!(store.read_work_item("work-1").unwrap(), work_item);
    assert_eq!(store.list_work_items().unwrap(), vec![work_item]);
}

#[test]
fn work_model_store_preserves_attempt_append_order() {
    let temp = tempfile::tempdir().unwrap();
    let store = WorkModelStore::new(temp.path());
    let mut work_item = WorkItem {
        id: "work-1".to_string(),
        title: "Order attempts".to_string(),
        planning_context: None,
        instructions: None,
        attempts: Vec::new(),
        merge_candidates: Vec::new(),
    };

    work_item.add_initial_attempt("attempt-2").unwrap();
    work_item.add_initial_attempt("attempt-10").unwrap();
    store.write_work_item(&work_item).unwrap();

    let stored_attempt = fs::read_to_string(
        temp.path()
            .join(".factory/work/attempts/work-1/attempt-10.json"),
    )
    .unwrap();
    let read = store.read_work_item("work-1").unwrap();

    assert!(stored_attempt.contains(r#""order": 1"#));
    assert_eq!(read.attempts[0].id, "attempt-2");
    assert_eq!(read.attempts[1].id, "attempt-10");
}

#[test]
fn work_model_store_preserves_task_append_order() {
    let temp = tempfile::tempdir().unwrap();
    let store = WorkModelStore::new(temp.path());
    let mut work_item = work_item();
    let mut followup = task(TaskKind::Write);
    followup.id = "write-followup".to_string();
    followup.status = TaskStatus::Planned;
    followup.output = None;
    let mut review = task(TaskKind::Review);
    review.id = "custom-review".to_string();
    review.role = "custom".to_string();
    review.workspace_access.reads = vec![workspace("candidate")];
    review.workspace_access.writes.clear();
    review.output = None;
    review.review_context = Some(ReviewContext {
        candidate_workspace_id: "candidate".to_string(),
        candidate_workspace_path: "../workspaces/candidate".to_string(),
        source_branch: "main".to_string(),
        candidate_commit: "abc123".to_string(),
    });
    work_item.attempts[0].tasks = vec![followup.clone(), review.clone()];
    work_item.attempts[0].status = AttemptStatus::Reviewing;

    store.write_work_item(&work_item).unwrap();

    let stored_task = fs::read_to_string(
        temp.path()
            .join(".factory/work/tasks/work-1/attempt-1/custom-review.json"),
    )
    .unwrap();
    let read = store.read_work_item("work-1").unwrap();

    assert!(stored_task.contains(r#""order": 1"#));
    assert_eq!(read.attempts[0].tasks[0].id, "write-followup");
    assert_eq!(read.attempts[0].tasks[1].id, "custom-review");
}

#[test]
fn work_model_store_rejects_duplicate_task_order() {
    let temp = tempfile::tempdir().unwrap();
    let store = WorkModelStore::new(temp.path());
    let mut work_item = work_item();
    let mut followup = task(TaskKind::Write);
    followup.id = "write-followup".to_string();
    followup.status = TaskStatus::Planned;
    followup.output = None;
    work_item.attempts[0].status = AttemptStatus::Executing;
    work_item.attempts[0].review_state = None;
    work_item.attempts[0].tasks.push(followup);

    store.write_work_item(&work_item).unwrap();

    let duplicate_path = temp
        .path()
        .join(".factory/work/tasks/work-1/attempt-1/write-followup.json");
    let mut duplicate: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&duplicate_path).unwrap()).unwrap();
    duplicate["order"] = serde_json::json!(0);
    fs::write(
        &duplicate_path,
        serde_json::to_string_pretty(&duplicate).unwrap() + "\n",
    )
    .unwrap();

    let error = store.read_work_item("work-1").unwrap_err();

    match error {
        WorkModelStorageError::InvalidModel { path, source } => {
            assert_eq!(path, duplicate_path);
            assert_eq!(
                source,
                WorkModelError::TaskOrderAlreadyExists {
                    attempt_id: "attempt-1".to_string(),
                    order: 0,
                }
            );
        }
        other => panic!("unexpected error: {other}"),
    }
}

#[test]
fn work_model_store_ignores_empty_split_directories_for_legacy_items() {
    let temp = tempfile::tempdir().unwrap();
    let store = WorkModelStore::new(temp.path());
    let work_item = work_item();
    let items_dir = temp.path().join(".factory/work/items");
    fs::create_dir_all(&items_dir).unwrap();
    fs::write(
        items_dir.join("work-1.json"),
        serde_json::to_string_pretty(&work_item).unwrap(),
    )
    .unwrap();
    fs::create_dir_all(temp.path().join(".factory/work/attempts/work-1")).unwrap();
    fs::create_dir_all(temp.path().join(".factory/work/tasks/work-1/attempt-1")).unwrap();
    fs::create_dir_all(temp.path().join(".factory/work/merge-candidates/work-1")).unwrap();

    assert_eq!(store.read_work_item("work-1").unwrap(), work_item);
    assert_eq!(store.list_work_items().unwrap(), vec![work_item]);
}

#[test]
fn work_model_store_create_refuses_existing_work_item() {
    let temp = tempfile::tempdir().unwrap();
    let store = WorkModelStore::new(temp.path());
    let original = work_item();
    let mut replacement = work_item();
    replacement.title = "Replacement title".to_string();

    store.create_work_item(&original).unwrap();
    let error = store.create_work_item(&replacement).unwrap_err();

    match error {
        WorkModelStorageError::WorkItemAlreadyExists { path, id } => {
            assert_eq!(path, temp.path().join(".factory/work/items/work-1.json"));
            assert_eq!(id, "work-1");
        }
        other => panic!("unexpected error: {other}"),
    }
    assert_eq!(store.read_work_item("work-1").unwrap(), original);
}

#[test]
fn work_model_store_keeps_existing_run_state_separate() {
    let temp = tempfile::tempdir().unwrap();
    let run_dir = temp.path().join(".factory/runs/run-legacy");
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("status"), "complete").unwrap();
    fs::write(run_dir.join("sessions.log"), "legacy session\n").unwrap();

    let store = WorkModelStore::new(temp.path());
    let work_item = work_item();

    store.write_work_item(&work_item).unwrap();

    assert_eq!(
        fs::read_to_string(run_dir.join("status")).unwrap(),
        "complete"
    );
    assert_eq!(
        fs::read_to_string(run_dir.join("sessions.log")).unwrap(),
        "legacy session\n"
    );
    assert!(temp.path().join(".factory/work/items/work-1.json").exists());
    assert_eq!(store.read_work_item("work-1").unwrap(), work_item);
}

#[test]
fn work_model_store_rejects_file_name_id_mismatch() {
    let temp = tempfile::tempdir().unwrap();
    let store = WorkModelStore::new(temp.path());
    store.write_work_item(&work_item()).unwrap();

    let path = temp.path().join(".factory/work/items/work-1.json");
    let invalid = fs::read_to_string(&path)
        .unwrap()
        .replace(r#""id": "work-1""#, r#""id": "work-2""#);
    fs::write(&path, invalid).unwrap();

    let error = store.read_work_item("work-1").unwrap_err();

    match error {
        WorkModelStorageError::WorkItemIdMismatch {
            path: actual,
            expected,
            actual: id,
        } => {
            assert_eq!(actual, path);
            assert_eq!(expected, "work-1");
            assert_eq!(id, "work-2");
        }
        other => panic!("unexpected error: {other}"),
    }

    assert!(matches!(
        store.list_work_items().unwrap_err(),
        WorkModelStorageError::WorkItemIdMismatch { .. }
    ));
}

#[test]
fn work_model_store_rejects_ids_that_cannot_name_files() {
    let temp = tempfile::tempdir().unwrap();
    let store = WorkModelStore::new(temp.path());

    for id in ["", ".", "..", "nested/work", r"nested\work"] {
        assert!(matches!(
            store.work_item_path(id).unwrap_err(),
            WorkModelStorageError::InvalidWorkItemId { .. }
        ));
        assert!(matches!(
            store.read_work_item(id).unwrap_err(),
            WorkModelStorageError::InvalidWorkItemId { .. }
        ));

        let mut work_item = work_item();
        work_item.id = id.to_string();
        assert!(matches!(
            store.write_work_item(&work_item).unwrap_err(),
            WorkModelStorageError::InvalidWorkItemId { .. }
        ));
        assert!(matches!(
            store.create_work_item(&work_item).unwrap_err(),
            WorkModelStorageError::InvalidWorkItemId { .. }
        ));
    }
}

#[test]
fn work_model_store_rejects_invalid_stored_file_stems() {
    let temp = tempfile::tempdir().unwrap();
    let store = WorkModelStore::new(temp.path());
    let items_dir = temp.path().join(".factory/work/items");
    fs::create_dir_all(&items_dir).unwrap();
    fs::write(
        items_dir.join(r"nested\work.json"),
        r#"{
  "id": "nested\\work",
  "title": "Invalid stored id",
  "attempts": []
}
"#,
    )
    .unwrap();

    assert!(matches!(
        store.list_work_items().unwrap_err(),
        WorkModelStorageError::InvalidWorkItemId { .. }
    ));
}

#[test]
fn work_model_store_writes_deterministic_pretty_json() {
    let temp = tempfile::tempdir().unwrap();
    let store = WorkModelStore::new(temp.path());

    store.write_work_item(&work_item()).unwrap();

    let path = temp.path().join(".factory/work/items/work-1.json");
    let content = fs::read_to_string(path).unwrap();
    assert_eq!(
        content,
        r#"{
  "id": "work-1",
  "title": "Add durable model storage"
}
"#
    );
    let attempt = fs::read_to_string(
        temp.path()
            .join(".factory/work/attempts/work-1/attempt-1.json"),
    )
    .unwrap();
    assert!(attempt.contains(r#""id": "attempt-1""#));
    assert!(attempt.contains(r#""status": "complete""#));
    assert!(!attempt.contains(r#""tasks""#));

    let task = fs::read_to_string(
        temp.path()
            .join(".factory/work/tasks/work-1/attempt-1/write-code.json"),
    )
    .unwrap();
    assert!(task.contains(r#""order": 0"#));
    assert!(task.contains(r#""id": "write-code""#));
    assert!(task.contains(r#""kind": "write""#));
    assert!(task.contains(r#""output": {"#));
}

#[test]
fn work_model_store_writes_merge_candidates_as_records() {
    let temp = tempfile::tempdir().unwrap();
    let store = WorkModelStore::new(temp.path());
    let mut item = work_item();
    item.merge_candidates.push(merge_candidate());

    store.write_work_item(&item).unwrap();

    let item_content =
        fs::read_to_string(temp.path().join(".factory/work/items/work-1.json")).unwrap();
    assert!(!item_content.contains(r#""merge_candidates""#));
    let content = fs::read_to_string(
        temp.path()
            .join(".factory/work/merge-candidates/work-1/attempt-1-merge-candidate.json"),
    )
    .unwrap();
    assert!(content.contains(r#""id": "attempt-1-merge-candidate""#));
    assert!(content.contains(r#""target_workspace": {"#));
    assert!(content.contains(r#""source_branch": "main""#));
    assert!(content.contains(r#""candidate_commit": "abc123""#));
    assert_eq!(store.read_work_item("work-1").unwrap(), item);
}

#[test]
fn work_model_store_prunes_stale_split_records() {
    let temp = tempfile::tempdir().unwrap();
    let store = WorkModelStore::new(temp.path());
    let mut item = work_item();
    item.merge_candidates.push(merge_candidate());

    store.write_work_item(&item).unwrap();

    let task_path = temp
        .path()
        .join(".factory/work/tasks/work-1/attempt-1/write-code.json");
    let attempt_path = temp
        .path()
        .join(".factory/work/attempts/work-1/attempt-1.json");
    let task_attempt_dir = temp.path().join(".factory/work/tasks/work-1/attempt-1");
    let candidate_path = temp
        .path()
        .join(".factory/work/merge-candidates/work-1/attempt-1-merge-candidate.json");
    assert!(task_path.exists());
    assert!(attempt_path.exists());
    assert!(candidate_path.exists());

    let mut without_task = item.clone();
    without_task.merge_candidates.clear();
    without_task.attempts[0].status = AttemptStatus::Planned;
    without_task.attempts[0].review_state = None;
    without_task.attempts[0].tasks.clear();
    store.write_work_item(&without_task).unwrap();

    assert!(!task_path.exists());
    assert!(!candidate_path.exists());
    assert!(attempt_path.exists());
    assert_eq!(store.read_work_item("work-1").unwrap(), without_task);

    let mut without_attempt = without_task.clone();
    without_attempt.attempts.clear();
    store.write_work_item(&without_attempt).unwrap();

    assert!(!attempt_path.exists());
    assert!(!task_attempt_dir.exists());
    assert_eq!(store.read_work_item("work-1").unwrap(), without_attempt);
}

#[test]
fn work_model_store_returns_empty_list_without_work_state() {
    let temp = tempfile::tempdir().unwrap();
    let store = WorkModelStore::new(temp.path());

    assert!(store.list_work_items().unwrap().is_empty());
}

#[test]
fn work_model_store_reports_file_for_invalid_json() {
    let temp = tempfile::tempdir().unwrap();
    let items = temp.path().join(".factory/work/items");
    fs::create_dir_all(&items).unwrap();
    let path = items.join("work-1.json");
    fs::write(&path, "{").unwrap();

    let error = WorkModelStore::new(temp.path())
        .read_work_item("work-1")
        .unwrap_err();

    match error {
        WorkModelStorageError::ParseFile { path: actual, .. } => assert_eq!(actual, path),
        other => panic!("unexpected error: {other}"),
    }
}

#[test]
fn work_model_store_reports_file_for_invalid_task_model() {
    let temp = tempfile::tempdir().unwrap();
    let store = WorkModelStore::new(temp.path());
    let mut invalid = work_item();
    invalid.attempts[0].tasks[0].kind = TaskKind::Review;

    let error = store.write_work_item(&invalid).unwrap_err();

    match error {
        WorkModelStorageError::InvalidModel { path, source } => {
            assert_eq!(path, temp.path().join(".factory/work/items/work-1.json"));
            assert_eq!(
                source,
                WorkModelError::ReviewTaskWritesWorkspace {
                    task_id: "write-code".to_string()
                }
            );
        }
        other => panic!("unexpected error: {other}"),
    }
}

#[test]
fn work_model_store_rejects_complete_write_task_without_output() {
    let temp = tempfile::tempdir().unwrap();
    let store = WorkModelStore::new(temp.path());
    let mut invalid = work_item();
    invalid.attempts[0].tasks[0].output = None;

    let error = store.write_work_item(&invalid).unwrap_err();

    match error {
        WorkModelStorageError::InvalidModel { path, source } => {
            assert_eq!(path, temp.path().join(".factory/work/items/work-1.json"));
            assert_eq!(
                source,
                WorkModelError::CompleteWriteTaskMissingOutput {
                    task_id: "write-code".to_string()
                }
            );
        }
        other => panic!("unexpected error: {other}"),
    }
}

#[test]
fn work_model_store_rejects_output_on_incomplete_task() {
    let temp = tempfile::tempdir().unwrap();
    let store = WorkModelStore::new(temp.path());
    let mut invalid = work_item();
    invalid.attempts[0].status = AttemptStatus::Executing;
    invalid.attempts[0].tasks[0].status = TaskStatus::Executing;

    let error = store.write_work_item(&invalid).unwrap_err();

    match error {
        WorkModelStorageError::InvalidModel { path, source } => {
            assert_eq!(path, temp.path().join(".factory/work/items/work-1.json"));
            assert_eq!(
                source,
                WorkModelError::IncompleteTaskHasOutput {
                    task_id: "write-code".to_string(),
                    status: TaskStatus::Executing,
                }
            );
        }
        other => panic!("unexpected error: {other}"),
    }
}

#[test]
fn work_model_store_rejects_complete_attempt_with_incomplete_task() {
    let temp = tempfile::tempdir().unwrap();
    let store = WorkModelStore::new(temp.path());
    let mut invalid = work_item();
    invalid.attempts[0].tasks[0].status = TaskStatus::Failed;
    invalid.attempts[0].tasks[0].output = None;

    let error = store.write_work_item(&invalid).unwrap_err();

    match error {
        WorkModelStorageError::InvalidModel { path, source } => {
            assert_eq!(path, temp.path().join(".factory/work/items/work-1.json"));
            assert_eq!(
                source,
                WorkModelError::CompleteAttemptHasIncompleteTask {
                    attempt_id: "attempt-1".to_string(),
                    task_id: "write-code".to_string(),
                    task_status: TaskStatus::Failed,
                }
            );
        }
        other => panic!("unexpected error: {other}"),
    }
}

#[test]
fn work_item_add_initial_attempt_creates_scheduler_facing_write_task() {
    let mut work_item = WorkItem {
        id: "work-1".to_string(),
        title: "Add attempt intake".to_string(),
        planning_context: None,
        instructions: None,
        attempts: Vec::new(),
        merge_candidates: Vec::new(),
    };

    work_item.add_initial_attempt("attempt-1").unwrap();

    assert_eq!(work_item.attempts.len(), 1);
    let attempt = &work_item.attempts[0];
    assert_eq!(attempt.id, "attempt-1");
    assert_eq!(attempt.work_item_id, "work-1");
    assert_eq!(attempt.status, AttemptStatus::Planned);
    assert_eq!(attempt.review_state, None);
    assert!(attempt.artifacts.is_empty());
    assert_eq!(attempt.tasks.len(), 1);

    let task = &attempt.tasks[0];
    assert_eq!(task.id, "attempt-1-write");
    assert_eq!(task.kind, TaskKind::Write);
    assert_eq!(task.role, "author");
    assert_eq!(task.work_item_id, "work-1");
    assert_eq!(task.attempt_id.as_deref(), Some("attempt-1"));
    assert!(task.workspace_access.reads.is_empty());
    assert_eq!(
        task.workspace_access.writes,
        vec![WorkspaceRef {
            id: "candidate".to_string(),
            path: "../work-6-work-1-attempt-1".to_string(),
        }]
    );
    assert_eq!(task.artifact_area, None);
    work_item.validate().unwrap();
}

#[test]
fn work_item_add_initial_attempt_appends_to_existing_attempts() {
    let mut work_item = work_item();
    let existing = work_item.attempts[0].clone();

    work_item.add_initial_attempt("attempt-2").unwrap();

    assert_eq!(work_item.attempts.len(), 2);
    assert_eq!(work_item.attempts[0], existing);

    let attempt = &work_item.attempts[1];
    assert_eq!(attempt.id, "attempt-2");
    assert_eq!(attempt.tasks.len(), 1);
    assert_eq!(attempt.tasks[0].id, "attempt-2-write");
    assert_eq!(attempt.tasks[0].attempt_id.as_deref(), Some("attempt-2"));
    assert_eq!(
        attempt.tasks[0].workspace_access.writes,
        vec![WorkspaceRef {
            id: "candidate".to_string(),
            path: "../work-6-work-1-attempt-2".to_string(),
        }]
    );
    work_item.validate().unwrap();
}

#[test]
fn work_item_add_initial_attempt_rejects_duplicate_attempt_id() {
    let mut work_item = WorkItem {
        id: "work-1".to_string(),
        title: "Add attempt intake".to_string(),
        planning_context: None,
        instructions: None,
        attempts: Vec::new(),
        merge_candidates: Vec::new(),
    };

    work_item.add_initial_attempt("attempt-1").unwrap();

    assert_eq!(
        work_item.add_initial_attempt("attempt-1").unwrap_err(),
        WorkModelError::AttemptAlreadyExists {
            id: "attempt-1".to_string(),
        }
    );
    assert_eq!(work_item.attempts.len(), 1);
}

#[test]
fn work_item_add_initial_attempt_rejects_invalid_attempt_id() {
    let mut work_item = WorkItem {
        id: "work-1".to_string(),
        title: "Add attempt intake".to_string(),
        planning_context: None,
        instructions: None,
        attempts: Vec::new(),
        merge_candidates: Vec::new(),
    };

    assert_eq!(
        work_item.add_initial_attempt("../escape").unwrap_err(),
        WorkModelError::InvalidId {
            kind: "attempt",
            id: "../escape".to_string(),
        }
    );
    assert!(work_item.attempts.is_empty());
}

#[test]
fn work_item_validate_rejects_attempt_with_wrong_work_item() {
    let mut invalid = work_item();
    invalid.attempts[0].work_item_id = "work-2".to_string();

    assert_eq!(
        invalid.validate().unwrap_err(),
        WorkModelError::AttemptWorkItemMismatch {
            attempt_id: "attempt-1".to_string(),
            expected: "work-1".to_string(),
            actual: "work-2".to_string(),
        }
    );
}

#[test]
fn work_item_validate_rejects_task_with_wrong_work_item() {
    let mut invalid = work_item();
    invalid.attempts[0].tasks[0].work_item_id = "work-2".to_string();

    assert_eq!(
        invalid.validate().unwrap_err(),
        WorkModelError::TaskWorkItemMismatch {
            task_id: "write-code".to_string(),
            expected: "work-1".to_string(),
            actual: "work-2".to_string(),
        }
    );
}

#[test]
fn work_item_validate_rejects_task_without_attempt() {
    let mut invalid = work_item();
    invalid.attempts[0].tasks[0].attempt_id = None;

    assert_eq!(
        invalid.validate().unwrap_err(),
        WorkModelError::TaskAttemptMismatch {
            task_id: "write-code".to_string(),
            expected: "attempt-1".to_string(),
            actual: None,
        }
    );
}

#[test]
fn work_item_validate_rejects_task_with_wrong_attempt() {
    let mut invalid = work_item();
    invalid.attempts[0].tasks[0].attempt_id = Some("attempt-2".to_string());

    assert_eq!(
        invalid.validate().unwrap_err(),
        WorkModelError::TaskAttemptMismatch {
            task_id: "write-code".to_string(),
            expected: "attempt-1".to_string(),
            actual: Some("attempt-2".to_string()),
        }
    );
}

#[test]
fn work_model_store_reports_file_for_invalid_model_read_from_disk() {
    let temp = tempfile::tempdir().unwrap();
    let store = WorkModelStore::new(temp.path());
    store.write_work_item(&work_item()).unwrap();

    let path = temp
        .path()
        .join(".factory/work/tasks/work-1/attempt-1/write-code.json");
    let invalid = fs::read_to_string(&path)
        .unwrap()
        .replace(r#""kind": "write""#, r#""kind": "review""#);
    fs::write(&path, invalid).unwrap();

    let error = store.read_work_item("work-1").unwrap_err();

    match error {
        WorkModelStorageError::InvalidModel {
            path: actual,
            source,
        } => {
            assert_eq!(actual, path);
            assert_eq!(
                source,
                WorkModelError::ReviewTaskWritesWorkspace {
                    task_id: "write-code".to_string(),
                }
            );
        }
        other => panic!("unexpected error: {other}"),
    }
}

#[test]
fn work_model_store_reports_file_for_split_attempt_id_mismatch() {
    let temp = tempfile::tempdir().unwrap();
    let store = WorkModelStore::new(temp.path());
    store.write_work_item(&work_item()).unwrap();

    let path = temp
        .path()
        .join(".factory/work/attempts/work-1/attempt-1.json");
    let invalid = fs::read_to_string(&path)
        .unwrap()
        .replace(r#""id": "attempt-1""#, r#""id": "attempt-2""#);
    fs::write(&path, invalid).unwrap();

    match store.read_work_item("work-1").unwrap_err() {
        WorkModelStorageError::InvalidModel {
            path: actual,
            source,
        } => {
            assert_eq!(actual, path);
            assert_eq!(
                source,
                WorkModelError::AttemptNotFound {
                    id: "attempt-1".to_string(),
                }
            );
        }
        other => panic!("unexpected error: {other}"),
    }
}

#[test]
fn work_model_store_reports_file_for_split_attempt_work_item_mismatch() {
    let temp = tempfile::tempdir().unwrap();
    let store = WorkModelStore::new(temp.path());
    store.write_work_item(&work_item()).unwrap();

    let path = temp
        .path()
        .join(".factory/work/attempts/work-1/attempt-1.json");
    let invalid = fs::read_to_string(&path)
        .unwrap()
        .replace(r#""work_item_id": "work-1""#, r#""work_item_id": "work-2""#);
    fs::write(&path, invalid).unwrap();

    match store.read_work_item("work-1").unwrap_err() {
        WorkModelStorageError::InvalidModel {
            path: actual,
            source,
        } => {
            assert_eq!(actual, path);
            assert_eq!(
                source,
                WorkModelError::AttemptWorkItemMismatch {
                    attempt_id: "attempt-1".to_string(),
                    expected: "work-1".to_string(),
                    actual: "work-2".to_string(),
                }
            );
        }
        other => panic!("unexpected error: {other}"),
    }
}

#[test]
fn work_model_store_reports_file_for_split_task_id_mismatch() {
    let temp = tempfile::tempdir().unwrap();
    let store = WorkModelStore::new(temp.path());
    store.write_work_item(&work_item()).unwrap();

    let path = temp
        .path()
        .join(".factory/work/tasks/work-1/attempt-1/write-code.json");
    let invalid = fs::read_to_string(&path)
        .unwrap()
        .replace(r#""id": "write-code""#, r#""id": "review-code""#);
    fs::write(&path, invalid).unwrap();

    match store.read_work_item("work-1").unwrap_err() {
        WorkModelStorageError::InvalidModel {
            path: actual,
            source,
        } => {
            assert_eq!(actual, path);
            assert_eq!(
                source,
                WorkModelError::TaskAlreadyExists {
                    id: "write-code".to_string(),
                }
            );
        }
        other => panic!("unexpected error: {other}"),
    }
}

#[test]
fn work_model_store_reports_file_for_split_task_work_item_mismatch() {
    let temp = tempfile::tempdir().unwrap();
    let store = WorkModelStore::new(temp.path());
    store.write_work_item(&work_item()).unwrap();

    let path = temp
        .path()
        .join(".factory/work/tasks/work-1/attempt-1/write-code.json");
    let invalid = fs::read_to_string(&path)
        .unwrap()
        .replace(r#""work_item_id": "work-1""#, r#""work_item_id": "work-2""#);
    fs::write(&path, invalid).unwrap();

    match store.read_work_item("work-1").unwrap_err() {
        WorkModelStorageError::InvalidModel {
            path: actual,
            source,
        } => {
            assert_eq!(actual, path);
            assert_eq!(
                source,
                WorkModelError::TaskWorkItemMismatch {
                    task_id: "write-code".to_string(),
                    expected: "work-1".to_string(),
                    actual: "work-2".to_string(),
                }
            );
        }
        other => panic!("unexpected error: {other}"),
    }
}

#[test]
fn work_model_store_reports_file_for_split_task_attempt_mismatch() {
    let temp = tempfile::tempdir().unwrap();
    let store = WorkModelStore::new(temp.path());
    store.write_work_item(&work_item()).unwrap();

    let path = temp
        .path()
        .join(".factory/work/tasks/work-1/attempt-1/write-code.json");
    let invalid = fs::read_to_string(&path).unwrap().replace(
        r#""attempt_id": "attempt-1""#,
        r#""attempt_id": "attempt-2""#,
    );
    fs::write(&path, invalid).unwrap();

    match store.read_work_item("work-1").unwrap_err() {
        WorkModelStorageError::InvalidModel {
            path: actual,
            source,
        } => {
            assert_eq!(actual, path);
            assert_eq!(
                source,
                WorkModelError::TaskAttemptMismatch {
                    task_id: "write-code".to_string(),
                    expected: "attempt-1".to_string(),
                    actual: Some("attempt-2".to_string()),
                }
            );
        }
        other => panic!("unexpected error: {other}"),
    }
}

#[test]
fn work_model_store_reports_file_for_split_merge_candidate_id_mismatch() {
    let temp = tempfile::tempdir().unwrap();
    let store = WorkModelStore::new(temp.path());
    let mut item = work_item();
    item.merge_candidates.push(merge_candidate());
    store.write_work_item(&item).unwrap();

    let path = temp
        .path()
        .join(".factory/work/merge-candidates/work-1/attempt-1-merge-candidate.json");
    let invalid = fs::read_to_string(&path).unwrap().replace(
        r#""id": "attempt-1-merge-candidate""#,
        r#""id": "attempt-1-other-candidate""#,
    );
    fs::write(&path, invalid).unwrap();

    match store.read_work_item("work-1").unwrap_err() {
        WorkModelStorageError::InvalidModel {
            path: actual,
            source,
        } => {
            assert_eq!(actual, path);
            assert_eq!(
                source,
                WorkModelError::MergeCandidateAlreadyExists {
                    id: "attempt-1-merge-candidate".to_string(),
                }
            );
        }
        other => panic!("unexpected error: {other}"),
    }
}

#[test]
fn work_model_store_reports_file_for_split_merge_candidate_attempt_mismatch() {
    let temp = tempfile::tempdir().unwrap();
    let store = WorkModelStore::new(temp.path());
    let mut item = work_item();
    item.merge_candidates.push(merge_candidate());
    store.write_work_item(&item).unwrap();

    let path = temp
        .path()
        .join(".factory/work/merge-candidates/work-1/attempt-1-merge-candidate.json");
    let invalid = fs::read_to_string(&path).unwrap().replace(
        r#""attempt_id": "attempt-1""#,
        r#""attempt_id": "attempt-2""#,
    );
    fs::write(&path, invalid).unwrap();

    match store.read_work_item("work-1").unwrap_err() {
        WorkModelStorageError::InvalidModel {
            path: actual,
            source,
        } => {
            assert_eq!(actual, path);
            assert_eq!(
                source,
                WorkModelError::MergeCandidateAttemptNotFound {
                    candidate_id: "attempt-1-merge-candidate".to_string(),
                    attempt_id: "attempt-2".to_string(),
                }
            );
        }
        other => panic!("unexpected error: {other}"),
    }
}
