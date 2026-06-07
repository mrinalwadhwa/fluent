use factory::work_model::{Task, TaskKind, WorkModelError, from_json};

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
