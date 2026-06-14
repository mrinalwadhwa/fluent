use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct UsageRow {
    pub ts: String,
    pub coder: String,
    pub work_item_id: String,
    pub attempt_id: String,
    pub task_id: String,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_output_tokens: Option<u64>,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct UsageSummary {
    pub per_coder: HashMap<String, CoderSummary>,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct CoderSummary {
    pub five_hour_spent: u64,
    pub weekly_spent: u64,
    pub window_recomputed_at: Option<String>,
}

pub fn usage_log_path() -> Result<PathBuf> {
    let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE"))?;
    Ok(PathBuf::from(home)
        .join(".config")
        .join("factory")
        .join("usage")
        .join("usage.jsonl"))
}

pub fn summary_path() -> Result<PathBuf> {
    let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE"))?;
    Ok(PathBuf::from(home)
        .join(".config")
        .join("factory")
        .join("usage")
        .join("summary.json"))
}

pub fn append_rows(rows: &[UsageRow]) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let path = usage_log_path()?;
    fs::create_dir_all(path.parent().unwrap())?;
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    for row in rows {
        writeln!(f, "{}", serde_json::to_string(row)?)?;
    }
    Ok(())
}

#[cfg(test)]
fn append_rows_to(path: &Path, rows: &[UsageRow]) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    fs::create_dir_all(path.parent().unwrap())?;
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    for row in rows {
        writeln!(f, "{}", serde_json::to_string(row)?)?;
    }
    Ok(())
}

pub fn recompute_summary() -> Result<UsageSummary> {
    recompute_summary_at(&usage_log_path()?, &summary_path()?)
}

fn recompute_summary_at(log_path: &Path, summary_out: &Path) -> Result<UsageSummary> {
    let now = chrono::Utc::now();
    let five_hours_ago = now - chrono::Duration::hours(5);
    let seven_days_ago = now - chrono::Duration::days(7);
    let now_str = now.to_rfc3339();

    let mut per_coder: HashMap<String, CoderSummary> = HashMap::new();

    if log_path.is_file() {
        let content = fs::read_to_string(log_path)?;
        for line in content.lines() {
            let row: UsageRow = match serde_json::from_str(line) {
                Ok(r) => r,
                Err(_) => continue,
            };
            let ts = match chrono::DateTime::parse_from_rfc3339(&row.ts) {
                Ok(dt) => dt.with_timezone(&chrono::Utc),
                Err(_) => continue,
            };
            let total = row.input_tokens + row.output_tokens;
            let entry = per_coder.entry(row.coder.clone()).or_default();
            if ts >= five_hours_ago {
                entry.five_hour_spent += total;
            }
            if ts >= seven_days_ago {
                entry.weekly_spent += total;
            }
            entry.window_recomputed_at = Some(now_str.clone());
        }
    }

    let summary = UsageSummary { per_coder };
    fs::create_dir_all(summary_out.parent().unwrap())?;
    fs::write(summary_out, serde_json::to_string_pretty(&summary)?)?;
    Ok(summary)
}

pub fn extract_claude_usage(
    transcript_path: &Path,
    work_item_id: &str,
    attempt_id: &str,
    task_id: &str,
) -> Vec<UsageRow> {
    let content = match fs::read_to_string(transcript_path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let mut rows = Vec::new();
    for line in content.lines() {
        let val: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        if val["type"].as_str() != Some("result") {
            continue;
        }

        let usage = &val["usage"];
        let input_tokens = match usage["input_tokens"].as_u64() {
            Some(n) => n,
            None => continue,
        };
        let output_tokens = match usage["output_tokens"].as_u64() {
            Some(n) => n,
            None => continue,
        };

        let model = val["model"]
            .as_str()
            .or_else(|| val["session_model"].as_str())
            .unwrap_or("unknown")
            .to_string();

        rows.push(UsageRow {
            ts: chrono::Utc::now().to_rfc3339(),
            coder: "claude".to_string(),
            work_item_id: work_item_id.to_string(),
            attempt_id: attempt_id.to_string(),
            task_id: task_id.to_string(),
            model,
            input_tokens,
            output_tokens,
            cached_input_tokens: usage["cache_read_input_tokens"]
                .as_u64()
                .or_else(|| usage["cached_input_tokens"].as_u64())
                .unwrap_or(0),
            reasoning_output_tokens: None,
        });
    }

    rows
}

pub fn extract_codex_usage(
    transcript_path: &Path,
    work_item_id: &str,
    attempt_id: &str,
    task_id: &str,
) -> Vec<UsageRow> {
    let content = match fs::read_to_string(transcript_path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let mut rows = Vec::new();
    for line in content.lines() {
        let val: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let is_token_count = val["type"].as_str() == Some("event_msg")
            && val["payload"]["type"].as_str() == Some("token_count");
        if !is_token_count {
            continue;
        }

        let last_usage = &val["payload"]["info"]["last_token_usage"];
        let input_tokens = match last_usage["input_tokens"].as_u64() {
            Some(n) => n,
            None => continue,
        };
        let output_tokens = match last_usage["output_tokens"].as_u64() {
            Some(n) => n,
            None => continue,
        };

        let model = val["payload"]["info"]["model"]
            .as_str()
            .or_else(|| val["payload"]["model"].as_str())
            .unwrap_or("unknown")
            .to_string();

        rows.push(UsageRow {
            ts: chrono::Utc::now().to_rfc3339(),
            coder: "codex".to_string(),
            work_item_id: work_item_id.to_string(),
            attempt_id: attempt_id.to_string(),
            task_id: task_id.to_string(),
            model,
            input_tokens,
            output_tokens,
            cached_input_tokens: last_usage["cached_input_tokens"].as_u64().unwrap_or(0),
            reasoning_output_tokens: last_usage["reasoning_output_tokens"].as_u64(),
        });
    }

    rows
}

pub fn log_usage_from_transcript(
    transcript_path: &Path,
    coder: &str,
    work_item_id: &str,
    attempt_id: &str,
    task_id: &str,
) {
    let rows = match coder {
        "claude" => extract_claude_usage(transcript_path, work_item_id, attempt_id, task_id),
        "codex" => extract_codex_usage(transcript_path, work_item_id, attempt_id, task_id),
        _ => return,
    };

    if rows.is_empty() {
        return;
    }

    if let Err(e) = append_rows(&rows) {
        eprintln!("warning: usage logging failed: {e}");
    }
    if let Err(e) = recompute_summary() {
        eprintln!("warning: usage summary update failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_row_round_trips_json() {
        let row = UsageRow {
            ts: "2026-06-13T10:00:00Z".to_string(),
            coder: "claude".to_string(),
            work_item_id: "wi-1".to_string(),
            attempt_id: "attempt-1".to_string(),
            task_id: "task-1".to_string(),
            model: "claude-opus-4-6".to_string(),
            input_tokens: 1000,
            output_tokens: 500,
            cached_input_tokens: 200,
            reasoning_output_tokens: Some(100),
        };
        let json = serde_json::to_string(&row).unwrap();
        let parsed: UsageRow = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.ts, row.ts);
        assert_eq!(parsed.coder, row.coder);
        assert_eq!(parsed.input_tokens, row.input_tokens);
        assert_eq!(parsed.output_tokens, row.output_tokens);
        assert_eq!(parsed.cached_input_tokens, row.cached_input_tokens);
        assert_eq!(parsed.reasoning_output_tokens, Some(100));
    }

    #[test]
    fn usage_row_omits_reasoning_when_none() {
        let row = UsageRow {
            ts: "2026-06-13T10:00:00Z".to_string(),
            coder: "claude".to_string(),
            work_item_id: "wi-1".to_string(),
            attempt_id: "attempt-1".to_string(),
            task_id: "task-1".to_string(),
            model: "claude-opus-4-6".to_string(),
            input_tokens: 1000,
            output_tokens: 500,
            cached_input_tokens: 0,
            reasoning_output_tokens: None,
        };
        let json = serde_json::to_string(&row).unwrap();
        assert!(!json.contains("reasoning_output_tokens"));
        let parsed: UsageRow = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.reasoning_output_tokens, None);
    }

    #[test]
    fn append_rows_creates_parent_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("deep").join("usage.jsonl");
        let row = UsageRow {
            ts: "2026-06-13T10:00:00Z".to_string(),
            coder: "claude".to_string(),
            work_item_id: "wi-1".to_string(),
            attempt_id: "attempt-1".to_string(),
            task_id: "task-1".to_string(),
            model: "claude-opus-4-6".to_string(),
            input_tokens: 100,
            output_tokens: 50,
            cached_input_tokens: 0,
            reasoning_output_tokens: None,
        };
        append_rows_to(&path, &[row]).unwrap();
        assert!(path.exists());
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("wi-1"));
    }

    #[test]
    fn append_rows_is_no_op_for_empty_slice() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.jsonl");
        append_rows_to(&path, &[]).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn recompute_summary_filters_by_five_hour_window() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("usage.jsonl");
        let summary_out = dir.path().join("summary.json");

        let now = chrono::Utc::now();
        let recent = (now - chrono::Duration::hours(1)).to_rfc3339();
        let old = (now - chrono::Duration::hours(6)).to_rfc3339();

        let rows = vec![
            UsageRow {
                ts: recent,
                coder: "claude".to_string(),
                work_item_id: "wi-1".to_string(),
                attempt_id: "a1".to_string(),
                task_id: "t1".to_string(),
                model: "m".to_string(),
                input_tokens: 100,
                output_tokens: 50,
                cached_input_tokens: 0,
                reasoning_output_tokens: None,
            },
            UsageRow {
                ts: old,
                coder: "claude".to_string(),
                work_item_id: "wi-1".to_string(),
                attempt_id: "a1".to_string(),
                task_id: "t1".to_string(),
                model: "m".to_string(),
                input_tokens: 200,
                output_tokens: 100,
                cached_input_tokens: 0,
                reasoning_output_tokens: None,
            },
        ];
        append_rows_to(&log_path, &rows).unwrap();
        let summary = recompute_summary_at(&log_path, &summary_out).unwrap();
        let claude = summary.per_coder.get("claude").unwrap();
        assert_eq!(claude.five_hour_spent, 150);
        assert_eq!(claude.weekly_spent, 450);
    }

    #[test]
    fn recompute_summary_filters_by_weekly_window() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("usage.jsonl");
        let summary_out = dir.path().join("summary.json");

        let now = chrono::Utc::now();
        let recent = (now - chrono::Duration::days(3)).to_rfc3339();
        let old = (now - chrono::Duration::days(10)).to_rfc3339();

        let rows = vec![
            UsageRow {
                ts: recent,
                coder: "codex".to_string(),
                work_item_id: "wi-2".to_string(),
                attempt_id: "a1".to_string(),
                task_id: "t1".to_string(),
                model: "m".to_string(),
                input_tokens: 500,
                output_tokens: 200,
                cached_input_tokens: 0,
                reasoning_output_tokens: None,
            },
            UsageRow {
                ts: old,
                coder: "codex".to_string(),
                work_item_id: "wi-2".to_string(),
                attempt_id: "a1".to_string(),
                task_id: "t1".to_string(),
                model: "m".to_string(),
                input_tokens: 1000,
                output_tokens: 500,
                cached_input_tokens: 0,
                reasoning_output_tokens: None,
            },
        ];
        append_rows_to(&log_path, &rows).unwrap();
        let summary = recompute_summary_at(&log_path, &summary_out).unwrap();
        let codex = summary.per_coder.get("codex").unwrap();
        assert_eq!(codex.five_hour_spent, 0);
        assert_eq!(codex.weekly_spent, 700);
    }

    #[test]
    fn recompute_summary_creates_zero_summary_when_log_missing() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("nonexistent.jsonl");
        let summary_out = dir.path().join("summary.json");
        let summary = recompute_summary_at(&log_path, &summary_out).unwrap();
        assert!(summary.per_coder.is_empty());
        assert!(summary_out.exists());
    }

    #[test]
    fn extract_claude_usage_returns_one_row_per_result_event() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        fs::write(
            &path,
            concat!(
                r#"{"type":"system","subtype":"init","session_id":"s1","model":"claude-opus-4-6"}"#,
                "\n",
                r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Working..."}]}}"#,
                "\n",
                r#"{"type":"result","model":"claude-opus-4-6","usage":{"input_tokens":1000,"output_tokens":500,"cache_read_input_tokens":200},"duration_ms":5000}"#,
                "\n",
                r#"{"type":"result","model":"claude-opus-4-6","usage":{"input_tokens":2000,"output_tokens":800,"cache_read_input_tokens":300},"duration_ms":3000}"#,
                "\n"
            ),
        )
        .unwrap();

        let rows = extract_claude_usage(&path, "wi-1", "attempt-1", "task-1");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].input_tokens, 1000);
        assert_eq!(rows[0].output_tokens, 500);
        assert_eq!(rows[0].cached_input_tokens, 200);
        assert_eq!(rows[0].coder, "claude");
        assert_eq!(rows[1].input_tokens, 2000);
        assert_eq!(rows[1].output_tokens, 800);
    }

    #[test]
    fn extract_claude_usage_returns_empty_when_no_result_events() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        fs::write(
            &path,
            concat!(
                r#"{"type":"system","subtype":"init","session_id":"s1","model":"claude-opus-4-6"}"#,
                "\n",
                r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Done."}]}}"#,
                "\n"
            ),
        )
        .unwrap();

        let rows = extract_claude_usage(&path, "wi-1", "attempt-1", "task-1");
        assert!(rows.is_empty());
    }

    #[test]
    fn extract_claude_usage_populates_model_field() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        fs::write(
            &path,
            r#"{"type":"result","model":"claude-sonnet-4-5","usage":{"input_tokens":100,"output_tokens":50},"duration_ms":1000}"#,
        )
        .unwrap();

        let rows = extract_claude_usage(&path, "wi-1", "attempt-1", "task-1");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].model, "claude-sonnet-4-5");
    }

    #[test]
    fn extract_claude_usage_skips_malformed_lines_silently() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        fs::write(
            &path,
            concat!(
                "not valid json\n",
                r#"{"type":"result","usage":{"input_tokens":100,"output_tokens":50}}"#,
                "\n",
                r#"{"type":"result","no_usage":true}"#,
                "\n"
            ),
        )
        .unwrap();

        let rows = extract_claude_usage(&path, "wi-1", "attempt-1", "task-1");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].input_tokens, 100);
    }

    #[test]
    fn extract_codex_usage_returns_one_row_per_token_count_event() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        fs::write(
            &path,
            concat!(
                r#"{"type":"session.meta","session_id":"s1"}"#,
                "\n",
                r#"{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":500,"output_tokens":200,"cached_input_tokens":100,"reasoning_output_tokens":50},"model":"o3"}}}"#,
                "\n",
                r#"{"type":"event_msg","payload":{"type":"response.item","item":{"type":"message"}}}"#,
                "\n",
                r#"{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":800,"output_tokens":300,"cached_input_tokens":0},"model":"o3"}}}"#,
                "\n"
            ),
        )
        .unwrap();

        let rows = extract_codex_usage(&path, "wi-2", "attempt-1", "task-1");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].input_tokens, 500);
        assert_eq!(rows[0].output_tokens, 200);
        assert_eq!(rows[0].cached_input_tokens, 100);
        assert_eq!(rows[0].reasoning_output_tokens, Some(50));
        assert_eq!(rows[0].coder, "codex");
        assert_eq!(rows[1].input_tokens, 800);
        assert_eq!(rows[1].reasoning_output_tokens, None);
    }

    #[test]
    fn extract_codex_usage_populates_reasoning_output_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        fs::write(
            &path,
            r#"{"type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":100,"output_tokens":50,"reasoning_output_tokens":30},"model":"o3"}}}"#,
        )
        .unwrap();

        let rows = extract_codex_usage(&path, "wi-1", "a1", "t1");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].reasoning_output_tokens, Some(30));
    }

    #[test]
    fn extract_codex_usage_skips_session_meta_and_response_item_events() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        fs::write(
            &path,
            concat!(
                r#"{"type":"session.meta","session_id":"s1"}"#,
                "\n",
                r#"{"type":"event_msg","payload":{"type":"response.item","item":{"type":"message"}}}"#,
                "\n",
                r#"{"type":"turn.completed"}"#,
                "\n"
            ),
        )
        .unwrap();

        let rows = extract_codex_usage(&path, "wi-1", "a1", "t1");
        assert!(rows.is_empty());
    }

    #[test]
    fn extract_claude_usage_returns_empty_for_missing_file() {
        let rows = extract_claude_usage(Path::new("/nonexistent"), "wi-1", "a1", "t1");
        assert!(rows.is_empty());
    }

    #[test]
    fn extract_codex_usage_returns_empty_for_missing_file() {
        let rows = extract_codex_usage(Path::new("/nonexistent"), "wi-1", "a1", "t1");
        assert!(rows.is_empty());
    }
}
