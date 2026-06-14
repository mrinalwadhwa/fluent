use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct QueueEntry {
    pub work_item_id: String,
    pub queued_at: String,
    pub priority: i64,
    pub status: QueueStatus,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum QueueStatus {
    Queued,
    Running,
    Done,
    Failed,
    NeedsUser,
}

impl std::fmt::Display for QueueStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Queued => write!(f, "queued"),
            Self::Running => write!(f, "running"),
            Self::Done => write!(f, "done"),
            Self::Failed => write!(f, "failed"),
            Self::NeedsUser => write!(f, "needs-user"),
        }
    }
}

fn queue_dir(project_root: &Path) -> PathBuf {
    project_root.join(".factory").join("work").join("queue")
}

fn queue_file(project_root: &Path, work_item_id: &str) -> PathBuf {
    queue_dir(project_root).join(format!("{work_item_id}.json"))
}

fn work_item_exists(project_root: &Path, id: &str) -> bool {
    project_root
        .join(".factory")
        .join("work")
        .join("items")
        .join(format!("{id}.json"))
        .is_file()
}

pub fn add(project_root: &Path, id: &str, priority: Option<i64>) -> Result<()> {
    if !work_item_exists(project_root, id) {
        bail!("Work Item {id:?} not found");
    }

    let path = queue_file(project_root, id);
    if path.is_file() {
        if let Some(new_priority) = priority {
            let content = fs::read_to_string(&path)?;
            let mut entry: QueueEntry = serde_json::from_str(&content)?;
            entry.priority = new_priority;
            fs::write(&path, serde_json::to_string_pretty(&entry)?)?;
        }
        return Ok(());
    }

    let entry = QueueEntry {
        work_item_id: id.to_string(),
        queued_at: chrono::Utc::now().to_rfc3339(),
        priority: priority.unwrap_or(0),
        status: QueueStatus::Queued,
    };
    fs::create_dir_all(queue_dir(project_root))?;
    fs::write(&path, serde_json::to_string_pretty(&entry)?)?;
    Ok(())
}

pub fn list(project_root: &Path) -> Result<Vec<QueueEntry>> {
    let dir = queue_dir(project_root);
    if !dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut entries = Vec::new();
    for dir_entry in fs::read_dir(&dir)? {
        let dir_entry = dir_entry?;
        let path = dir_entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => {
                eprintln!(
                    "warning: could not read queue file {}",
                    path.display()
                );
                continue;
            }
        };
        match serde_json::from_str::<QueueEntry>(&content) {
            Ok(entry) => entries.push(entry),
            Err(e) => {
                eprintln!(
                    "warning: malformed queue file {}: {e}",
                    path.display()
                );
                continue;
            }
        }
    }

    entries.sort_by(|a, b| {
        b.priority.cmp(&a.priority).then_with(|| a.queued_at.cmp(&b.queued_at))
    });

    Ok(entries)
}

pub fn remove(project_root: &Path, id: &str) -> Result<()> {
    let path = queue_file(project_root, id);
    if !path.is_file() {
        bail!("Work Item {id:?} is not queued");
    }
    fs::remove_file(&path)?;
    Ok(())
}

pub fn update_status(project_root: &Path, id: &str, status: QueueStatus) -> Result<()> {
    let path = queue_file(project_root, id);
    if !path.is_file() {
        bail!("Queue entry {id:?} not found");
    }
    let content = fs::read_to_string(&path)?;
    let mut entry: QueueEntry = serde_json::from_str(&content)?;
    entry.status = status;
    fs::write(&path, serde_json::to_string_pretty(&entry)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_project(tmp: &Path) {
        fs::create_dir_all(tmp.join(".factory/work/items")).unwrap();
    }

    fn write_work_item(tmp: &Path, id: &str) {
        fs::write(
            tmp.join(format!(".factory/work/items/{id}.json")),
            format!(r#"{{"id": "{id}", "title": "Test"}}"#),
        )
        .unwrap();
    }

    #[test]
    fn add_writes_queued_entry_with_default_priority() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        write_work_item(dir.path(), "wi-1");

        add(dir.path(), "wi-1", None).unwrap();

        let path = queue_file(dir.path(), "wi-1");
        assert!(path.exists());
        let entry: QueueEntry = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(entry.work_item_id, "wi-1");
        assert_eq!(entry.priority, 0);
        assert_eq!(entry.status, QueueStatus::Queued);
    }

    #[test]
    fn add_idempotent_updates_priority_only_when_passed() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        write_work_item(dir.path(), "wi-1");

        add(dir.path(), "wi-1", Some(5)).unwrap();
        let entry1: QueueEntry =
            serde_json::from_str(&fs::read_to_string(queue_file(dir.path(), "wi-1")).unwrap())
                .unwrap();
        assert_eq!(entry1.priority, 5);
        let original_queued_at = entry1.queued_at.clone();

        add(dir.path(), "wi-1", Some(10)).unwrap();
        let entry2: QueueEntry =
            serde_json::from_str(&fs::read_to_string(queue_file(dir.path(), "wi-1")).unwrap())
                .unwrap();
        assert_eq!(entry2.priority, 10);
        assert_eq!(entry2.queued_at, original_queued_at);
    }

    #[test]
    fn add_idempotent_preserves_queued_at_on_existing_entry() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        write_work_item(dir.path(), "wi-1");

        add(dir.path(), "wi-1", None).unwrap();
        let entry1: QueueEntry =
            serde_json::from_str(&fs::read_to_string(queue_file(dir.path(), "wi-1")).unwrap())
                .unwrap();

        add(dir.path(), "wi-1", None).unwrap();
        let entry2: QueueEntry =
            serde_json::from_str(&fs::read_to_string(queue_file(dir.path(), "wi-1")).unwrap())
                .unwrap();

        assert_eq!(entry1.queued_at, entry2.queued_at);
    }

    #[test]
    fn add_fails_when_work_item_does_not_exist() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());

        let result = add(dir.path(), "nonexistent", None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn list_sorts_by_priority_then_queued_at() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());

        for id in &["wi-a", "wi-b", "wi-c"] {
            write_work_item(dir.path(), id);
        }

        let qdir = queue_dir(dir.path());
        fs::create_dir_all(&qdir).unwrap();

        fs::write(
            qdir.join("wi-a.json"),
            r#"{"work_item_id":"wi-a","queued_at":"2026-06-13T10:00:00Z","priority":5,"status":"queued"}"#,
        )
        .unwrap();
        fs::write(
            qdir.join("wi-b.json"),
            r#"{"work_item_id":"wi-b","queued_at":"2026-06-13T09:00:00Z","priority":10,"status":"queued"}"#,
        )
        .unwrap();
        fs::write(
            qdir.join("wi-c.json"),
            r#"{"work_item_id":"wi-c","queued_at":"2026-06-13T08:00:00Z","priority":5,"status":"queued"}"#,
        )
        .unwrap();

        let entries = list(dir.path()).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].work_item_id, "wi-b");
        assert_eq!(entries[1].work_item_id, "wi-c");
        assert_eq!(entries[2].work_item_id, "wi-a");
    }

    #[test]
    fn list_returns_empty_when_no_queue_files() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        let entries = list(dir.path()).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn remove_deletes_file() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        write_work_item(dir.path(), "wi-1");
        add(dir.path(), "wi-1", None).unwrap();

        remove(dir.path(), "wi-1").unwrap();
        assert!(!queue_file(dir.path(), "wi-1").exists());
    }

    #[test]
    fn remove_errors_when_not_queued() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        let result = remove(dir.path(), "wi-1");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not queued"));
    }

    #[test]
    fn update_status_transitions_through_states() {
        let dir = tempfile::tempdir().unwrap();
        setup_project(dir.path());
        write_work_item(dir.path(), "wi-1");
        add(dir.path(), "wi-1", None).unwrap();

        update_status(dir.path(), "wi-1", QueueStatus::Running).unwrap();
        let entry: QueueEntry =
            serde_json::from_str(&fs::read_to_string(queue_file(dir.path(), "wi-1")).unwrap())
                .unwrap();
        assert_eq!(entry.status, QueueStatus::Running);

        update_status(dir.path(), "wi-1", QueueStatus::Done).unwrap();
        let entry: QueueEntry =
            serde_json::from_str(&fs::read_to_string(queue_file(dir.path(), "wi-1")).unwrap())
                .unwrap();
        assert_eq!(entry.status, QueueStatus::Done);

        update_status(dir.path(), "wi-1", QueueStatus::Failed).unwrap();
        let entry: QueueEntry =
            serde_json::from_str(&fs::read_to_string(queue_file(dir.path(), "wi-1")).unwrap())
                .unwrap();
        assert_eq!(entry.status, QueueStatus::Failed);

        update_status(dir.path(), "wi-1", QueueStatus::NeedsUser).unwrap();
        let entry: QueueEntry =
            serde_json::from_str(&fs::read_to_string(queue_file(dir.path(), "wi-1")).unwrap())
                .unwrap();
        assert_eq!(entry.status, QueueStatus::NeedsUser);
    }
}
