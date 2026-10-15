use fluent::work_model::{
    Attempt, AttemptKind, AttemptReviewState, AttemptStatus, CoderMapping, DurableTransitionOrigin,
    LearnerMode, MergeCandidate, MergeCandidateMergeState, MergeReviewState, ReviewContext, Task,
    TaskArtifactArea, TaskKind, TaskOutput, TaskStatus, WorkItem, WorkModelError,
    WorkModelStorageError, WorkModelStore, WorkspaceAccess, WorkspaceRef, from_json,
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
            path: ".fluent/work/artifacts/write-code".to_string(),
        }),
        review_context: None,
        writer_correction_assignment: None,
        verification_context: None,
        evidence_review_context: None,
        input_artifacts: Vec::new(),
        depends_on: None,
        output: Some(TaskOutput {
            workspace_id: "candidate".to_string(),
            workspace_path: "../workspaces/candidate".to_string(),
            source_branch: "main".to_string(),
            base_commit: None,
            commit: "abc123".to_string(),
            no_change: None,
            learner_canonicalization: None,
        }),
        created_at: None,
        started_at: None,
        completed_at: None,
    }
}

fn work_item() -> WorkItem {
    WorkItem {
        id: "work-1".to_string(),
        title: "Add durable model storage".to_string(),
        planning_context: None,
        instructions: None,
        abandonment: None,
        post_merge_review_fix_depth: None,
        attempts: vec![Attempt {
            id: "attempt-1".to_string(),
            work_item_id: "work-1".to_string(),
            kind: AttemptKind::Write,
            status: AttemptStatus::Complete,
            coder_mapping: CoderMapping::default(),
            tasks: vec![task(TaskKind::Write)],
            review_state: Some(AttemptReviewState::Passed),
            pause_kind: None,
            artifacts: Vec::new(),
            created_at: None,
            completed_at: None,
            ..Default::default()
        }],
        merge_candidates: Vec::new(),
        ..Default::default()
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
        merge_review_state: MergeReviewState::Pending,
        merge_state: MergeCandidateMergeState::default(),
        created_at: None,
        ready_at: None,
        started_at: None,
        completed_at: None,
    }
}

fn assert_stale_write(error: WorkModelStorageError) {
    match error {
        WorkModelStorageError::StaleWorkItem {
            expected_revision,
            actual_revision,
            ..
        } => {
            assert_eq!(expected_revision, Some(0));
            assert_eq!(actual_revision, 1);
        }
        other => panic!("expected stale aggregate write, got {other}"),
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

    assert!(temp.path().join(".fluent/work/items/work-1.json").exists());
    assert!(
        temp.path()
            .join(".fluent/work/attempts/work-1/attempt-1.json")
            .exists()
    );
    assert!(
        temp.path()
            .join(".fluent/work/tasks/work-1/attempt-1/write-code.json")
            .exists()
    );
    assert!(!temp.path().join(".fluent/runs").exists());
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
        abandonment: None,
        post_merge_review_fix_depth: None,
        attempts: Vec::new(),
        merge_candidates: Vec::new(),
        ..Default::default()
    };

    work_item.add_initial_attempt("attempt-2").unwrap();
    work_item.add_initial_attempt("attempt-10").unwrap();
    store.write_work_item(&work_item).unwrap();

    let stored_attempt = fs::read_to_string(
        temp.path()
            .join(".fluent/work/attempts/work-1/attempt-10.json"),
    )
    .unwrap();
    let read = store.read_work_item("work-1").unwrap();

    assert!(stored_attempt.contains(r#""order": 1"#));
    assert_eq!(read.attempts[0].id, "attempt-2");
    assert_eq!(read.attempts[1].id, "attempt-10");
}

#[test]
fn stale_retry_cannot_revert_landed_candidate() {
    let temp = tempfile::tempdir().unwrap();
    let store = WorkModelStore::new(temp.path());
    let mut initial = work_item();
    initial.merge_candidates.push(merge_candidate());
    store.write_work_item(&initial).unwrap();
    let mut land = store.read_work_item("work-1").unwrap();
    let mut retry = store.read_work_item("work-1").unwrap();

    land.merge_candidates[0].merge_state.status =
        fluent::work_model::MergeCandidateMergeStatus::Merged;
    land.merge_candidates[0].merge_state.merged_commit = Some("merged123".to_string());
    store.write_work_item(&land).unwrap();
    retry.attempts[0].learning = Some(fluent::work_model::AttemptLearning::failed(1, "retry"));

    assert_stale_write(store.write_work_item(&retry).unwrap_err());
    let stored = store.read_work_item("work-1").unwrap();
    assert_eq!(
        stored.merge_candidates[0].merge_state.status,
        fluent::work_model::MergeCandidateMergeStatus::Merged
    );
    assert_eq!(
        stored.merge_candidates[0]
            .merge_state
            .merged_commit
            .as_deref(),
        Some("merged123")
    );
}

#[test]
fn stale_retry_cannot_replace_competing_retry_result() {
    let temp = tempfile::tempdir().unwrap();
    let store = WorkModelStore::new(temp.path());
    store.write_work_item(&work_item()).unwrap();
    let mut first = store.read_work_item("work-1").unwrap();
    let mut second = store.read_work_item("work-1").unwrap();

    first.attempts[0].learning = Some(fluent::work_model::AttemptLearning::failed(1, "first"));
    second.attempts[0].learning = Some(fluent::work_model::AttemptLearning::failed(1, "second"));
    store.write_work_item(&first).unwrap();

    assert_stale_write(store.write_work_item(&second).unwrap_err());
    assert_eq!(
        store.read_work_item("work-1").unwrap().attempts[0]
            .learning
            .as_ref()
            .unwrap()
            .last_failure
            .as_deref(),
        Some("first")
    );
}

#[test]
fn stale_task_write_cannot_revert_coder_mapping() {
    let temp = tempfile::tempdir().unwrap();
    let store = WorkModelStore::new(temp.path());
    store.write_work_item(&work_item()).unwrap();
    let mut mapping = store.read_work_item("work-1").unwrap();
    let mut task_update = store.read_work_item("work-1").unwrap();

    mapping.attempts[0].coder_mapping.write.model = "new-model".to_string();
    task_update.attempts[0].tasks[0]
        .output
        .as_mut()
        .unwrap()
        .commit = "task-update".to_string();
    store.write_work_item(&mapping).unwrap();

    assert_stale_write(store.write_work_item(&task_update).unwrap_err());
    let stored = store.read_work_item("work-1").unwrap();
    assert_eq!(stored.attempts[0].coder_mapping.write.model, "new-model");
    assert_eq!(
        stored.attempts[0].tasks[0].output.as_ref().unwrap().commit,
        "abc123"
    );
}

#[test]
fn stale_aggregate_write_cannot_prune_sibling_records() {
    let temp = tempfile::tempdir().unwrap();
    let store = WorkModelStore::new(temp.path());
    store.write_work_item(&work_item()).unwrap();
    let mut sibling_update = store.read_work_item("work-1").unwrap();
    let mut stale = store.read_work_item("work-1").unwrap();

    sibling_update.add_initial_attempt("attempt-2").unwrap();
    store.write_work_item(&sibling_update).unwrap();
    stale.title = "stale title".to_string();

    assert_stale_write(store.write_work_item(&stale).unwrap_err());
    let stored = store.read_work_item("work-1").unwrap();
    assert_eq!(stored.title, "Add durable model storage");
    assert!(
        stored
            .attempts
            .iter()
            .any(|attempt| attempt.id == "attempt-2")
    );
    assert!(
        temp.path()
            .join(".fluent/work/attempts/work-1/attempt-2.json")
            .exists()
    );
}

#[test]
fn unobserved_aggregate_cannot_replace_competing_creation() {
    let temp = tempfile::tempdir().unwrap();
    let store = WorkModelStore::new(temp.path());
    let first = WorkItem::planned("work-new", "first creation");
    let second = WorkItem::planned("work-new", "competing creation");

    store.write_work_item(&first).unwrap();
    let error = store.write_work_item(&second).unwrap_err();

    match error {
        WorkModelStorageError::StaleWorkItem {
            expected_revision,
            actual_revision,
            ..
        } => {
            assert_eq!(expected_revision, None);
            assert_eq!(actual_revision, 0);
        }
        other => panic!("expected competing creation to fail stale, got {other}"),
    }
    assert_eq!(
        store.read_work_item("work-new").unwrap().title,
        "first creation"
    );
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
        base_commit: None,
    });
    work_item.attempts[0].tasks = vec![followup.clone(), review.clone()];
    work_item.attempts[0].status = AttemptStatus::Reviewing;

    store.write_work_item(&work_item).unwrap();

    let stored_task = fs::read_to_string(
        temp.path()
            .join(".fluent/work/tasks/work-1/attempt-1/custom-review.json"),
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
        .join(".fluent/work/tasks/work-1/attempt-1/write-followup.json");
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
fn work_model_store_ignores_nested_operational_collections() {
    let temp = tempfile::tempdir().unwrap();
    let store = WorkModelStore::new(temp.path());

    let mut split_item = work_item();
    split_item.merge_candidates.push(merge_candidate());
    store.write_work_item(&split_item).unwrap();

    let mut nested_item = work_item();
    nested_item.attempts[0].id = "nested-attempt".to_string();
    nested_item.attempts[0].tasks[0].id = "nested-task".to_string();
    nested_item.merge_candidates.push(MergeCandidate {
        id: "nested-merge-candidate".to_string(),
        attempt_id: "nested-attempt".to_string(),
        ..merge_candidate()
    });
    fs::write(
        temp.path().join(".fluent/work/items/work-1.json"),
        serde_json::to_string_pretty(&nested_item).unwrap(),
    )
    .unwrap();

    assert_eq!(store.read_work_item("work-1").unwrap(), split_item);
    assert_eq!(store.list_work_items().unwrap(), vec![split_item]);
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
            assert_eq!(path, temp.path().join(".fluent/work/items/work-1.json"));
            assert_eq!(id, "work-1");
        }
        other => panic!("unexpected error: {other}"),
    }
    assert_eq!(store.read_work_item("work-1").unwrap(), original);
}

#[test]
fn work_model_store_keeps_existing_run_state_separate() {
    let temp = tempfile::tempdir().unwrap();
    let run_dir = temp.path().join(".fluent/runs/run-legacy");
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
    assert!(temp.path().join(".fluent/work/items/work-1.json").exists());
    assert_eq!(store.read_work_item("work-1").unwrap(), work_item);
}

#[test]
fn work_model_store_rejects_file_name_id_mismatch() {
    let temp = tempfile::tempdir().unwrap();
    let store = WorkModelStore::new(temp.path());
    store.write_work_item(&work_item()).unwrap();

    let path = temp.path().join(".fluent/work/items/work-1.json");
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
    let items_dir = temp.path().join(".fluent/work/items");
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

    let path = temp.path().join(".fluent/work/items/work-1.json");
    let content = fs::read_to_string(path).unwrap();
    assert_eq!(
        content,
        format!(
            r#"{{
  "schema_version": 14,
  "id": "work-1",
  "title": "Add durable model storage",
  "model_writer_version": {{
    "format": "fluent-work-item-writer",
    "schema_version": 12,
    "version": "{version}"
  }}
}}
"#,
            version = env!("CARGO_PKG_VERSION")
        )
    );
    let attempt = fs::read_to_string(
        temp.path()
            .join(".fluent/work/attempts/work-1/attempt-1.json"),
    )
    .unwrap();
    assert!(attempt.contains(r#""id": "attempt-1""#));
    assert!(attempt.contains(r#""status": "complete""#));
    assert!(!attempt.contains(r#""tasks""#));

    let task = fs::read_to_string(
        temp.path()
            .join(".fluent/work/tasks/work-1/attempt-1/write-code.json"),
    )
    .unwrap();
    assert!(task.contains(r#""order": 0"#));
    assert!(task.contains(r#""id": "write-code""#));
    assert!(task.contains(r#""kind": "write""#));
    assert!(task.contains(r#""output": {"#));
}

#[test]
fn capture_mode_preserves_minimal_work_item_json() {
    // The default `capture` Learner mode is omitted from the split record while
    // every successful write stamps the record schema and model writer versions.
    let temp = tempfile::tempdir().unwrap();
    let store = WorkModelStore::new(temp.path());

    let item = WorkItem {
        id: "work-1".to_string(),
        title: "Add durable model storage".to_string(),
        learner_mode: LearnerMode::Capture,
        ..Default::default()
    };
    store.write_work_item(&item).unwrap();

    let path = temp.path().join(".fluent/work/items/work-1.json");
    let content = fs::read_to_string(path).unwrap();
    assert_eq!(
        content,
        format!(
            r#"{{
  "schema_version": 14,
  "id": "work-1",
  "title": "Add durable model storage",
  "model_writer_version": {{
    "format": "fluent-work-item-writer",
    "schema_version": 12,
    "version": "{version}"
  }}
}}
"#,
            version = env!("CARGO_PKG_VERSION")
        )
    );
    assert!(!content.contains("learner_mode"));
}

#[test]
fn work_model_store_writes_merge_candidates_as_records() {
    let temp = tempfile::tempdir().unwrap();
    let store = WorkModelStore::new(temp.path());
    let mut item = work_item();
    item.merge_candidates.push(merge_candidate());

    store.write_work_item(&item).unwrap();

    let item_content =
        fs::read_to_string(temp.path().join(".fluent/work/items/work-1.json")).unwrap();
    assert!(!item_content.contains(r#""merge_candidates""#));
    let content = fs::read_to_string(
        temp.path()
            .join(".fluent/work/merge-candidates/work-1/attempt-1-merge-candidate.json"),
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
        .join(".fluent/work/tasks/work-1/attempt-1/write-code.json");
    let attempt_path = temp
        .path()
        .join(".fluent/work/attempts/work-1/attempt-1.json");
    let task_attempt_dir = temp.path().join(".fluent/work/tasks/work-1/attempt-1");
    let candidate_path = temp
        .path()
        .join(".fluent/work/merge-candidates/work-1/attempt-1-merge-candidate.json");
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
    let after_prune = store.read_work_item("work-1").unwrap();
    assert_eq!(
        after_prune
            .transitions
            .iter()
            .map(|transition| (
                transition.object_kind.as_str(),
                transition.object_id.as_str(),
                transition.old_state.as_deref(),
                transition.new_state.as_str(),
                transition.origin,
            ))
            .collect::<Vec<_>>(),
        vec![
            (
                "work-item",
                "work-1",
                None,
                "authorized",
                DurableTransitionOrigin::Unknown
            ),
            (
                "attempt",
                "attempt-1",
                None,
                "complete",
                DurableTransitionOrigin::Unknown
            ),
            (
                "task",
                "write-code",
                None,
                "complete",
                DurableTransitionOrigin::Unknown
            ),
            (
                "merge-candidate",
                "attempt-1-merge-candidate",
                None,
                "pending",
                DurableTransitionOrigin::Unknown,
            ),
            (
                "attempt",
                "attempt-1",
                Some("complete"),
                "planned",
                DurableTransitionOrigin::Fluent,
            ),
        ]
    );
    let mut business_state = after_prune.clone();
    business_state.transitions.clear();
    assert_eq!(business_state, without_task);

    let mut without_attempt = without_task.clone();
    without_attempt.attempts.clear();
    store.write_work_item(&without_attempt).unwrap();

    assert!(!attempt_path.exists());
    assert!(!task_attempt_dir.exists());
    let after_attempt_prune = store.read_work_item("work-1").unwrap();
    assert_eq!(after_attempt_prune.transitions, after_prune.transitions);
    let mut business_state = after_attempt_prune;
    business_state.transitions.clear();
    assert_eq!(business_state, without_attempt);
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
    let items = temp.path().join(".fluent/work/items");
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
            assert_eq!(path, temp.path().join(".fluent/work/items/work-1.json"));
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
            assert_eq!(path, temp.path().join(".fluent/work/items/work-1.json"));
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
            assert_eq!(path, temp.path().join(".fluent/work/items/work-1.json"));
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
            assert_eq!(path, temp.path().join(".fluent/work/items/work-1.json"));
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
        abandonment: None,
        post_merge_review_fix_depth: None,
        attempts: Vec::new(),
        merge_candidates: Vec::new(),
        ..Default::default()
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
    assert_eq!(task.id, "attempt-1-write-1");
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
    assert_eq!(
        task.artifact_area,
        Some(TaskArtifactArea {
            path: ".fluent/work/artifacts/work-1/attempt-1/attempt-1-write-1".to_string(),
        })
    );
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
    assert_eq!(attempt.tasks[0].id, "attempt-2-write-1");
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
        abandonment: None,
        post_merge_review_fix_depth: None,
        attempts: Vec::new(),
        merge_candidates: Vec::new(),
        ..Default::default()
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
        abandonment: None,
        post_merge_review_fix_depth: None,
        attempts: Vec::new(),
        merge_candidates: Vec::new(),
        ..Default::default()
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
        .join(".fluent/work/tasks/work-1/attempt-1/write-code.json");
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
        .join(".fluent/work/attempts/work-1/attempt-1.json");
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
        .join(".fluent/work/attempts/work-1/attempt-1.json");
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
        .join(".fluent/work/tasks/work-1/attempt-1/write-code.json");
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
        .join(".fluent/work/tasks/work-1/attempt-1/write-code.json");
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
        .join(".fluent/work/tasks/work-1/attempt-1/write-code.json");
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
        .join(".fluent/work/merge-candidates/work-1/attempt-1-merge-candidate.json");
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
        .join(".fluent/work/merge-candidates/work-1/attempt-1-merge-candidate.json");
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

// Include directories as well as bytes so observation cannot hide lock creation.
fn work_tree(
    root: &std::path::Path,
) -> std::collections::BTreeMap<std::path::PathBuf, Option<Vec<u8>>> {
    fn visit(
        root: &std::path::Path,
        path: &std::path::Path,
        tree: &mut std::collections::BTreeMap<std::path::PathBuf, Option<Vec<u8>>>,
    ) {
        if !path.exists() {
            return;
        }
        let bytes = if path.is_dir() {
            None
        } else {
            Some(fs::read(path).unwrap())
        };
        tree.insert(path.strip_prefix(root).unwrap().to_path_buf(), bytes);
        if path.is_dir() {
            for entry in fs::read_dir(path).unwrap() {
                visit(root, &entry.unwrap().path(), tree);
            }
        }
    }
    let mut tree = std::collections::BTreeMap::new();
    visit(root, &root.join(".fluent/work"), &mut tree);
    tree
}

fn pending_transaction(root: &std::path::Path, mut item: WorkItem, published: bool) {
    use sha2::Digest;
    let base_children_digest = published.then(|| {
        format!(
            "{:x}",
            sha2::Sha256::digest(
                serde_json::to_vec(&(&item.attempts, &item.merge_candidates)).unwrap()
            )
        )
    });
    item.title = "Unpublished title".into();
    let dir = root.join(".fluent/work/transactions");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(format!("{}.json", item.id)), serde_json::to_vec_pretty(&serde_json::json!({
        "schema_version": 1, "id": item.id, "base_revision": if published { Some(0) } else { None },
        "target_revision": if published { 1 } else { 0 }, "base_children_digest": base_children_digest, "work_item": item
    })).unwrap()).unwrap();
}

#[test]
fn status_snapshot_is_nonmutating() {
    let temp = tempfile::tempdir().unwrap();
    let store = WorkModelStore::new(temp.path());
    store.create_work_item(&work_item()).unwrap();
    let mut running = WorkItem::planned("running", "Observe running Task");
    running.add_initial_attempt("attempt-1").unwrap();
    running.attempts[0].status = AttemptStatus::Executing;
    running.attempts[0].tasks[0].status = TaskStatus::Executing;
    store.create_work_item(&running).unwrap();
    let area = &running.attempts[0].tasks[0]
        .artifact_area
        .as_ref()
        .unwrap()
        .path;
    fs::create_dir_all(temp.path().join(area)).unwrap();
    fs::write(
        temp.path().join(area).join("execution.json"),
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 1, "state": "running", "owner_pid": std::process::id(),
            "leader_pid": std::process::id(), "process_group_id": std::process::id(),
            "started_at": "2026-09-06T00:00:00Z", "heartbeat_at": "2026-09-06T00:00:00Z"
        }))
        .unwrap(),
    )
    .unwrap();
    pending_transaction(temp.path(), work_item(), true);
    fs::remove_dir_all(temp.path().join(".fluent/work/locks")).unwrap();
    let before = work_tree(temp.path());
    let status = fluent::work_status::load_work_status(temp.path()).unwrap();
    assert_cmd::Command::new(assert_cmd::cargo::cargo_bin!("fluent"))
        .current_dir(temp.path())
        .arg("status")
        .assert()
        .success();
    assert_eq!(work_tree(temp.path()), before);
    assert_eq!(
        status
            .rows
            .iter()
            .find(|row| row.id == "work-1")
            .unwrap()
            .title,
        "Add durable model storage"
    );
}

#[test]
fn snapshot_reports_pending_transaction_without_recovery() {
    let temp = tempfile::tempdir().unwrap();
    let store = WorkModelStore::new(temp.path());
    store.create_work_item(&work_item()).unwrap();
    pending_transaction(temp.path(), work_item(), true);
    pending_transaction(
        temp.path(),
        WorkItem::planned("unpublished", "Pending creation"),
        false,
    );
    let before = work_tree(temp.path());
    let snapshot = store.snapshot_work_items().unwrap();
    assert_eq!(snapshot.items.len(), 1);
    assert_eq!(snapshot.items[0].title, "Add durable model storage");
    assert_eq!(snapshot.diagnostics.len(), 2);
    for id in ["work-1", "unpublished"] {
        assert!(
            snapshot
                .diagnostics
                .iter()
                .any(|d| d.contains(id) && d.contains("host-owned recovery is pending")),
            "{:?}",
            snapshot.diagnostics
        );
    }
    assert_eq!(work_tree(temp.path()), before);
}

#[test]
fn snapshot_does_not_migrate_legacy_artifacts() {
    use std::os::unix::process::CommandExt;
    let temp = tempfile::tempdir().unwrap();
    let store = WorkModelStore::new(temp.path());
    let mut running = work_item();
    running.attempts[0].status = AttemptStatus::Executing;
    running.attempts[0].review_state = None;
    running.attempts[0].tasks[0].status = TaskStatus::Executing;
    running.attempts[0].tasks[0].output = None;
    store.create_work_item(&running).unwrap();
    let path = store
        .work_task_path("work-1", "attempt-1", "write-code")
        .unwrap();
    let mut record: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let legacy = ".fluent/work/artifacts/attempt-1/write-code";
    record["artifact_area"]["path"] = legacy.into();
    fs::write(path, serde_json::to_vec_pretty(&record).unwrap()).unwrap();
    let area = temp.path().join(legacy);
    fs::create_dir_all(&area).unwrap();
    let transcript = area.join("transcript.jsonl");
    fs::write(&transcript, "stored transcript").unwrap();
    assert!(
        !temp
            .path()
            .join(".fluent/work/artifacts/work-1/attempt-1/write-code")
            .exists()
    );
    assert!(!fluent::lease::task_lock_path(temp.path(), "work-1", "write-code").exists());

    // Keep a real execution identity only in the stored legacy artifact area.
    // Piped stdin holds cat open without spawning descendants or a heartbeat.
    let mut command = std::process::Command::new("/bin/cat");
    command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .process_group(0);
    fluent::execution::configure_identity_lock(&mut command, &transcript).unwrap();
    let mut child = command.spawn().unwrap();
    let execution = fluent::execution::ExecutionRecord {
        schema_version: 1,
        state: fluent::execution::ExecutionState::Running,
        owner_pid: std::process::id(),
        leader_pid: child.id(),
        process_group_id: child.id(),
        started_at: "2026-09-06T00:00:00Z".into(),
        heartbeat_at: "2026-09-06T00:00:00Z".into(),
        settled_at: None,
        attempt_owner: None,
        launch_generation: None,
    };
    fs::write(
        area.join("execution.json"),
        serde_json::to_vec(&execution).unwrap(),
    )
    .unwrap();
    let before = work_tree(temp.path());
    let status = fluent::work_status::load_work_status(temp.path());
    let snapshot = store.snapshot_work_items();
    let after = work_tree(temp.path());
    child.kill().unwrap();
    child.wait().unwrap();

    let status = status.unwrap();
    assert!(status.errors.is_empty(), "{:?}", status.errors);
    assert_eq!(status.rows[0].task, "write:write-code [executing]");
    assert_eq!(status.rows[0].action, "executing");
    let snapshot = snapshot.unwrap();
    assert!(
        snapshot.diagnostics.is_empty(),
        "{:?}",
        snapshot.diagnostics
    );
    assert_eq!(
        snapshot.items[0].attempts[0].tasks[0]
            .artifact_area
            .as_ref()
            .unwrap()
            .path,
        legacy
    );
    assert_eq!(after, before);
    // Releasing the same legacy identity must change status without changing files.
    let stopped = fluent::work_status::load_work_status(temp.path()).unwrap();
    assert_eq!(stopped.rows[0].task, "write:write-code [interrupted]");
    assert_eq!(work_tree(temp.path()), before);
}

#[test]
fn snapshot_isolates_item_and_transaction_errors() {
    let temp = tempfile::tempdir().unwrap();
    let store = WorkModelStore::new(temp.path());
    store
        .create_work_item(&WorkItem::planned("valid", "Visible"))
        .unwrap();
    store.create_work_item(&work_item()).unwrap();
    fs::write(
        store
            .work_task_path("work-1", "attempt-1", "write-code")
            .unwrap(),
        "bad task",
    )
    .unwrap();
    fs::write(store.work_item_path("broken").unwrap(), "bad item").unwrap();
    let dir = temp.path().join(".fluent/work/transactions");
    fs::write(dir.join("bad name.json"), "bad filename").unwrap();
    fs::write(dir.join("broken-journal.json"), "bad journal").unwrap();
    let before = work_tree(temp.path());
    let snapshot = store.snapshot_work_items().unwrap();
    assert_eq!(snapshot.items.len(), 1);
    assert_eq!(snapshot.items[0].id, "valid");
    for name in [
        "write-code.json",
        "broken.json",
        "bad name",
        "broken-journal",
    ] {
        assert!(
            snapshot.diagnostics.iter().any(|d| d.contains(name)),
            "{:?}",
            snapshot.diagnostics
        );
    }
    assert!(snapshot.diagnostics.iter().all(|d| d.len() <= 512));
    assert_eq!(work_tree(temp.path()), before);
}

#[test]
fn current_record_has_old_reader_wire_fence() {
    let temp = tempfile::tempdir().unwrap();
    let store = WorkModelStore::new(temp.path());
    store.create_work_item(&work_item()).unwrap();
    let record: serde_json::Value =
        serde_json::from_slice(&fs::read(store.work_item_path("work-1").unwrap()).unwrap())
            .unwrap();
    assert_eq!(
        record["model_writer_version"],
        serde_json::json!({
            "format": "fluent-work-item-writer", "schema_version": 12,
            "version": env!("CARGO_PKG_VERSION")
        })
    );
    // The actual release executable is exercised by test-work-model-old-reader-fence.sh.
    assert_eq!(
        store
            .read_work_item_snapshot("work-1")
            .unwrap()
            .model_writer_version
            .as_deref(),
        Some(env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn current_reader_accepts_legacy_writer_string_without_rewrite() {
    let temp = tempfile::tempdir().unwrap();
    let store = WorkModelStore::new(temp.path());
    store.create_work_item(&work_item()).unwrap();
    for path in [
        store.work_item_path("work-1").unwrap(),
        store.work_attempt_path("work-1", "attempt-1").unwrap(),
    ] {
        let mut record: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        record.as_object_mut().unwrap().remove("schema_version");
        if record.get("model_writer_version").is_some() {
            record["model_writer_version"] = "0.3.0".into();
        }
        fs::write(path, serde_json::to_vec_pretty(&record).unwrap()).unwrap();
    }
    let before = work_tree(temp.path());
    let item = store.read_work_item_snapshot("work-1").unwrap();
    assert_eq!(item.model_writer_version.as_deref(), Some("0.3.0"));
    assert_eq!(item.attempts, work_item().attempts);
    let status = fluent::work_status::load_work_status(temp.path()).unwrap();
    assert!(status.errors.is_empty());
    assert_eq!(status.rows.len(), 1);
    assert_eq!(work_tree(temp.path()), before);
}

#[test]
fn current_reader_round_trips_fenced_record() {
    let temp = tempfile::tempdir().unwrap();
    let store = WorkModelStore::new(temp.path());
    let mut initial = work_item();
    initial.learner_mode = LearnerMode::NoExpertise;
    initial.post_merge_review_fix_depth = Some(2);
    initial.merge_candidates.push(merge_candidate());
    store.create_work_item(&initial).unwrap();
    let stale = store.read_work_item_snapshot("work-1").unwrap();
    store
        .mutate_work_item("work-1", |item| {
            item.title = "Updated fenced record".into();
            Ok(())
        })
        .unwrap();
    let mut expected = stale.clone();
    expected.title = "stale update".into();
    assert_stale_write(store.write_work_item(&expected).unwrap_err());
    let current = store.read_work_item_snapshot("work-1").unwrap();
    assert_eq!(current.title, "Updated fenced record");
    assert_eq!(current.attempts, initial.attempts);
    assert_eq!(current.merge_candidates, initial.merge_candidates);
    assert_eq!(current.learner_mode, initial.learner_mode);
    assert_eq!(
        current.post_merge_review_fix_depth,
        initial.post_merge_review_fix_depth
    );
    let before = work_tree(temp.path());
    store.write_work_item(&current).unwrap();
    assert_eq!(work_tree(temp.path()), before);
    assert!(
        !temp
            .path()
            .join(".fluent/work/transactions/work-1.json")
            .exists()
    );
}

#[test]
fn writer_envelope_rejects_unknown_or_malformed_values() {
    let temp = tempfile::tempdir().unwrap();
    let store = WorkModelStore::new(temp.path());
    store.create_work_item(&work_item()).unwrap();
    let path = store.work_item_path("work-1").unwrap();
    let mut record: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let future_schema = record["model_writer_version"]["schema_version"]
        .as_u64()
        .unwrap()
        + 100;
    for envelope in [
        serde_json::json!({"format": "other", "schema_version": 1, "version": "0.3.0"}),
        serde_json::json!({"format": "fluent-work-item-writer", "schema_version": future_schema, "version": "0.3.0"}),
        serde_json::json!({"format": "fluent-work-item-writer", "schema_version": 0, "version": "0.3.0"}),
        serde_json::json!({"format": "fluent-work-item-writer", "schema_version": 1, "version": "invalid"}),
        serde_json::json!({"format": "fluent-work-item-writer", "schema_version": 1}),
        serde_json::json!({"format": "fluent-work-item-writer", "schema_version": 1, "version": "0.3.0", "authority": "unexpected"}),
    ] {
        record["model_writer_version"] = envelope.clone();
        fs::write(&path, serde_json::to_vec_pretty(&record).unwrap()).unwrap();
        let before = work_tree(temp.path());
        assert!(
            store.read_work_item_snapshot("work-1").is_err(),
            "{envelope}"
        );
        assert!(store.mutate_work_item("work-1", |_| Ok(())).is_err());
        assert_eq!(work_tree(temp.path()), before);
    }
}

#[test]
fn explicit_upgrade_replaces_current_version_legacy_string() {
    let temp = tempfile::tempdir().unwrap();
    let store = WorkModelStore::new(temp.path());
    store.create_work_item(&work_item()).unwrap();
    let path = store.work_item_path("work-1").unwrap();
    let mut record: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    record["model_writer_version"] = env!("CARGO_PKG_VERSION").into();
    fs::write(&path, serde_json::to_vec_pretty(&record).unwrap()).unwrap();
    assert!(!store.upgrade_work_item("work-1").unwrap().already_current);
    let record: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert!(record["model_writer_version"].is_object());
    assert!(store.upgrade_work_item("work-1").unwrap().already_current);
}
