use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// A parsed event from a transcript JSONL file.
#[derive(Debug, Clone)]
pub enum Event {
    SessionInit {
        session_id: String,
        model: String,
    },
    ToolUse {
        id: String,
        name: String,
        summary: String,
    },
    Text {
        text: String,
    },
    Thinking {
        text: String,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
    },
    RateLimit,
    Result {
        duration_ms: Option<u64>,
        cost_usd: Option<f64>,
    },
    Unknown {
        event_type: String,
    },
}

impl Event {
    /// Render this event as one or more lines for the activity feed.
    pub fn lines(&self) -> Vec<String> {
        match self {
            Event::SessionInit { model, .. } => {
                vec![
                    String::new(),
                    format!("Session started (model: {model})"),
                    String::new(),
                ]
            }
            Event::ToolUse { name, summary, .. } => {
                let header = if summary.is_empty() {
                    format!("[{name}]")
                } else {
                    format!("[{name}] {summary}")
                };
                // Blank line before file operations for visual separation
                match name.as_str() {
                    "Read" | "Write" | "Edit" | "Bash" | "Grep" | "Glob" | "Agent" => {
                        vec![String::new(), header]
                    }
                    _ => vec![header],
                }
            }
            Event::Text { text } => {
                let mut lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();
                lines.push(String::new());
                lines
            }
            Event::Thinking { text } => {
                if text.is_empty() {
                    vec![String::new(), "thinking...".to_string()]
                } else {
                    let mut lines = vec![String::new(), "thinking...".to_string()];
                    for line in text.lines() {
                        lines.push(format!("  {line}"));
                    }
                    lines.push(String::new());
                    lines
                }
            }
            Event::ToolResult { content, .. } => {
                if content.is_empty() {
                    return vec![];
                }
                let lines: Vec<&str> = content.lines().collect();
                let max_lines = 20;
                let mut result: Vec<String> = lines
                    .iter()
                    .take(max_lines)
                    .map(|l| format!("  {l}"))
                    .collect();
                if lines.len() > max_lines {
                    result.push(format!("  ... ({} more lines)", lines.len() - max_lines));
                }
                result
            }
            Event::RateLimit => vec!["rate limit check".to_string()],
            Event::Result {
                duration_ms,
                cost_usd,
            } => {
                let dur = duration_ms
                    .map(|ms| format!("{:.1}s", ms as f64 / 1000.0))
                    .unwrap_or_else(|| "?".into());
                let cost = cost_usd
                    .map(|c| format!("${c:.4}"))
                    .unwrap_or_else(|| "?".into());
                vec![String::new(), format!("Session complete ({dur}, {cost})")]
            }
            Event::Unknown { event_type } => vec![format!("({event_type})")],
        }
    }

    /// Single-line summary (for backward compatibility and reviewer status).
    pub fn summary(&self) -> String {
        self.lines()
            .into_iter()
            .find(|l| !l.is_empty())
            .unwrap_or_default()
    }
}

/// Parse a single JSONL line into events. One line can produce multiple events
/// (e.g., an assistant message with both text and tool_use content blocks).
pub fn parse_line(line: &str) -> Vec<Event> {
    let Ok(val) = serde_json::from_str::<serde_json::Value>(line) else {
        return vec![];
    };

    let event_type = val["type"].as_str().unwrap_or("");

    match event_type {
        "system" => {
            let subtype = val["subtype"].as_str().unwrap_or("");
            if subtype == "init" {
                vec![Event::SessionInit {
                    session_id: val["session_id"].as_str().unwrap_or("").to_string(),
                    model: val["model"].as_str().unwrap_or("unknown").to_string(),
                }]
            } else {
                vec![Event::Unknown {
                    event_type: format!("system:{subtype}"),
                }]
            }
        }
        "assistant" => {
            let content = val["message"]["content"].as_array();
            let Some(blocks) = content else {
                return vec![];
            };
            let mut events = Vec::new();
            for block in blocks {
                let block_type = block["type"].as_str().unwrap_or("");
                match block_type {
                    "tool_use" => {
                        let id = block["id"].as_str().unwrap_or("").to_string();
                        let name = block["name"].as_str().unwrap_or("?").to_string();
                        let summary = summarize_tool_input(&name, &block["input"]);
                        events.push(Event::ToolUse { id, name, summary });
                    }
                    "text" => {
                        let text = block["text"].as_str().unwrap_or("").to_string();
                        if !text.is_empty() {
                            events.push(Event::Text { text });
                        }
                    }
                    "thinking" => {
                        let text = block["thinking"].as_str().unwrap_or("").to_string();
                        events.push(Event::Thinking { text });
                    }
                    _ => {}
                }
            }
            events
        }
        "user" => {
            let content = val["message"]["content"].as_array();
            let Some(blocks) = content else {
                return vec![];
            };
            let mut events = Vec::new();
            for block in blocks {
                if block["type"].as_str() == Some("tool_result") {
                    let id = block["tool_use_id"].as_str().unwrap_or("").to_string();
                    let content = extract_tool_result_content(block);
                    events.push(Event::ToolResult {
                        tool_use_id: id,
                        content,
                    });
                }
            }
            events
        }
        "rate_limit_event" => vec![Event::RateLimit],
        "result" => vec![Event::Result {
            duration_ms: val["duration_ms"].as_u64(),
            cost_usd: val["cost_usd"]
                .as_f64()
                .or_else(|| val["total_cost_usd"].as_f64()),
        }],
        "thread.started" => vec![Event::SessionInit {
            session_id: val["thread_id"].as_str().unwrap_or("").to_string(),
            model: "codex".to_string(),
        }],
        "turn.started" => vec![],
        "turn.completed" => vec![Event::Result {
            duration_ms: None,
            cost_usd: None,
        }],
        "item.started" | "item.completed" => parse_codex_item(event_type, &val["item"]),
        _ => vec![Event::Unknown {
            event_type: event_type.to_string(),
        }],
    }
}

/// Parse Codex CLI `--json` item events into the dashboard's event model.
fn parse_codex_item(event_type: &str, item: &serde_json::Value) -> Vec<Event> {
    let item_type = item["type"].as_str().unwrap_or("");
    match item_type {
        "agent_message" => {
            let text = item["text"].as_str().unwrap_or("").to_string();
            if text.is_empty() {
                vec![]
            } else {
                vec![Event::Text { text }]
            }
        }
        "command_execution" => {
            let id = item["id"].as_str().unwrap_or("").to_string();
            let command = item["command"].as_str().unwrap_or("").to_string();
            let output = item["aggregated_output"].as_str().unwrap_or("").to_string();
            match event_type {
                "item.started" => vec![Event::ToolUse {
                    id,
                    name: "Bash".to_string(),
                    summary: if command.is_empty() {
                        String::new()
                    } else {
                        format!("$ {command}")
                    },
                }],
                "item.completed" if !output.is_empty() => vec![Event::ToolResult {
                    tool_use_id: id,
                    content: output,
                }],
                _ => vec![],
            }
        }
        _ => vec![],
    }
}

/// Extract content from a tool_result block, preserving multiple lines.
fn extract_tool_result_content(block: &serde_json::Value) -> String {
    // Content can be a string or an array of content blocks
    if let Some(s) = block["content"].as_str() {
        s.to_string()
    } else if let Some(arr) = block["content"].as_array() {
        arr.iter()
            .filter_map(|item| {
                if item["type"].as_str() == Some("tool_result")
                    || item["type"].as_str() == Some("text")
                {
                    item["text"].as_str().map(|s| s.to_string())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    } else if block.get("is_error") == Some(&serde_json::Value::Bool(true)) {
        // Error results
        block["content"].as_str().unwrap_or("(error)").to_string()
    } else {
        String::new()
    }
}

/// Summarize tool input for display.
fn summarize_tool_input(tool_name: &str, input: &serde_json::Value) -> String {
    match tool_name {
        "Read" => input["file_path"].as_str().unwrap_or("").to_string(),
        "Edit" => input["file_path"].as_str().unwrap_or("").to_string(),
        "Write" => input["file_path"].as_str().unwrap_or("").to_string(),
        "Bash" => {
            let cmd = input["command"].as_str().unwrap_or("");
            let desc = input["description"].as_str().unwrap_or("");
            if !desc.is_empty() && !cmd.is_empty() {
                format!("{desc}\n  $ {cmd}")
            } else if !desc.is_empty() {
                desc.to_string()
            } else {
                format!("$ {cmd}")
            }
        }
        "Grep" => {
            let pattern = input["pattern"].as_str().unwrap_or("");
            let path = input["path"].as_str().unwrap_or("");
            if !path.is_empty() {
                format!("/{pattern}/ in {path}")
            } else {
                format!("/{pattern}/")
            }
        }
        "Glob" => input["pattern"].as_str().unwrap_or("").to_string(),
        "TodoWrite" => "update tasks".to_string(),
        "Agent" => {
            let desc = input["description"].as_str().unwrap_or("");
            desc.to_string()
        }
        _ => String::new(),
    }
}

/// Incrementally read new lines from a transcript file.
pub struct TranscriptReader {
    pub path: PathBuf,
    offset: u64,
}

impl TranscriptReader {
    pub fn new(path: PathBuf) -> Self {
        Self { path, offset: 0 }
    }

    /// Read any new lines since the last call. Returns parsed events.
    pub fn read_new(&mut self) -> Vec<Event> {
        let Ok(file) = File::open(&self.path) else {
            return vec![];
        };
        let mut reader = BufReader::new(file);
        if reader.seek(SeekFrom::Start(self.offset)).is_err() {
            return vec![];
        }

        let mut events = Vec::new();
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(n) => {
                    self.offset += n as u64;
                    let trimmed = line.trim();
                    if !trimmed.is_empty() {
                        events.extend(parse_line(trimmed));
                    }
                }
                Err(_) => break,
            }
        }
        events
    }
}

/// Find the latest transcript file for a run.
pub fn find_latest_transcript(run_dir: &Path) -> Option<PathBuf> {
    let sessions_dir = run_dir.join("sessions");
    if !sessions_dir.is_dir() {
        return None;
    }

    let mut max_num: u32 = 0;
    let mut best: Option<PathBuf> = None;

    if let Ok(entries) = std::fs::read_dir(&sessions_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(num_str) = name.strip_prefix("session-")
                && let Ok(num) = num_str.parse::<u32>()
            {
                let transcript = entry.path().join("transcript.jsonl");
                if transcript.exists() && num >= max_num {
                    max_num = num;
                    best = Some(transcript);
                }
            }
        }
    }

    best
}

/// List all session transcript paths for a run, ordered by session number.
pub fn list_transcripts(run_dir: &Path) -> Vec<(u32, PathBuf)> {
    let sessions_dir = run_dir.join("sessions");
    if !sessions_dir.is_dir() {
        return vec![];
    }

    let mut results = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&sessions_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(num_str) = name.strip_prefix("session-")
                && let Ok(num) = num_str.parse::<u32>()
            {
                let transcript = entry.path().join("transcript.jsonl");
                if transcript.exists() {
                    results.push((num, transcript));
                }
            }
        }
    }
    results.sort_by_key(|(n, _)| *n);
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn test_parse_system_init() {
        let line =
            r#"{"type":"system","subtype":"init","session_id":"abc","model":"claude-opus-4-6"}"#;
        let events = parse_line(line);
        assert_eq!(events.len(), 1);
        match &events[0] {
            Event::SessionInit { session_id, model } => {
                assert_eq!(session_id, "abc");
                assert_eq!(model, "claude-opus-4-6");
            }
            _ => panic!("Expected SessionInit"),
        }
    }

    #[test]
    fn test_parse_tool_use() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":"/foo/bar/baz.rs"}}]}}"#;
        let events = parse_line(line);
        assert_eq!(events.len(), 1);
        match &events[0] {
            Event::ToolUse { name, summary, .. } => {
                assert_eq!(name, "Read");
                assert!(summary.contains("baz.rs"));
            }
            _ => panic!("Expected ToolUse"),
        }
    }

    #[test]
    fn test_parse_text() {
        let line =
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Hello world"}]}}"#;
        let events = parse_line(line);
        assert_eq!(events.len(), 1);
        match &events[0] {
            Event::Text { text } => assert_eq!(text, "Hello world"),
            _ => panic!("Expected Text"),
        }
    }

    #[test]
    fn test_parse_thinking() {
        let line =
            r#"{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"hmm"}]}}"#;
        let events = parse_line(line);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], Event::Thinking { .. }));
    }

    #[test]
    fn test_parse_multiple_content_blocks() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"msg"},{"type":"tool_use","name":"Bash","input":{"command":"ls","description":"list files"}}]}}"#;
        let events = parse_line(line);
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], Event::Text { .. }));
        assert!(matches!(&events[1], Event::ToolUse { .. }));
    }

    #[test]
    fn test_parse_rate_limit() {
        let line = r#"{"type":"rate_limit_event"}"#;
        let events = parse_line(line);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], Event::RateLimit));
    }

    #[test]
    fn test_parse_result() {
        let line = r#"{"type":"result","duration_ms":5000,"total_cost_usd":0.05}"#;
        let events = parse_line(line);
        assert_eq!(events.len(), 1);
        match &events[0] {
            Event::Result {
                duration_ms,
                cost_usd,
            } => {
                assert_eq!(*duration_ms, Some(5000));
                assert!((cost_usd.unwrap() - 0.05).abs() < 0.001);
            }
            _ => panic!("Expected Result"),
        }
    }

    #[test]
    fn test_parse_invalid_json() {
        let events = parse_line("not json");
        assert!(events.is_empty());
    }

    #[test]
    fn test_parse_tool_result_is_silent() {
        let line =
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"123"}]}}"#;
        let events = parse_line(line);
        assert_eq!(events.len(), 1);
        // Tool results have empty summary
        assert!(events[0].summary().is_empty());
    }

    #[test]
    fn test_bash_summary_prefers_description() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"cargo build","description":"Build the project"}}]}}"#;
        let events = parse_line(line);
        match &events[0] {
            Event::ToolUse { summary, .. } => {
                assert!(summary.contains("Build the project"));
                assert!(summary.contains("cargo build"));
            }
            _ => panic!("Expected ToolUse"),
        }
    }

    #[test]
    fn test_bash_summary_falls_back_to_command() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"cargo build"}}]}}"#;
        let events = parse_line(line);
        match &events[0] {
            Event::ToolUse { summary, .. } => {
                assert_eq!(summary, "$ cargo build");
            }
            _ => panic!("Expected ToolUse"),
        }
    }

    #[test]
    fn test_grep_summary() {
        let input: serde_json::Value = serde_json::json!({"pattern": "fn main"});
        assert_eq!(summarize_tool_input("Grep", &input), "/fn main/");
    }

    #[test]
    fn test_glob_summary() {
        let input: serde_json::Value = serde_json::json!({"pattern": "**/*.rs"});
        assert_eq!(summarize_tool_input("Glob", &input), "**/*.rs");
    }

    #[test]
    fn test_agent_summary_shows_full_description() {
        let desc = "a".repeat(80);
        let input: serde_json::Value = serde_json::json!({
            "description": desc
        });
        let result = summarize_tool_input("Agent", &input);
        assert_eq!(result, desc);
    }

    #[test]
    fn test_edit_summary_shows_full_path() {
        let input: serde_json::Value = serde_json::json!({"file_path": "/a/b/c/d.rs"});
        assert_eq!(summarize_tool_input("Edit", &input), "/a/b/c/d.rs");
    }

    #[test]
    fn test_write_summary_shows_full_path() {
        let input: serde_json::Value = serde_json::json!({"file_path": "/a/b/c/d.rs"});
        assert_eq!(summarize_tool_input("Write", &input), "/a/b/c/d.rs");
    }

    #[test]
    fn test_todowrite_summary() {
        let input = serde_json::Value::Null;
        assert_eq!(summarize_tool_input("TodoWrite", &input), "update tasks");
    }

    #[test]
    fn test_unknown_tool_summary() {
        let input = serde_json::Value::Null;
        assert_eq!(summarize_tool_input("FooBar", &input), "");
    }

    #[test]
    fn test_parse_result_cost_usd_field() {
        let line = r#"{"type":"result","duration_ms":3000,"cost_usd":0.12}"#;
        let events = parse_line(line);
        match &events[0] {
            Event::Result { cost_usd, .. } => {
                assert!((cost_usd.unwrap() - 0.12).abs() < 0.001);
            }
            _ => panic!("Expected Result"),
        }
    }

    #[test]
    fn test_parse_codex_thread_started() {
        let line = r#"{"type":"thread.started","thread_id":"abc"}"#;
        let events = parse_line(line);
        assert_eq!(events.len(), 1);
        match &events[0] {
            Event::SessionInit { session_id, model } => {
                assert_eq!(session_id, "abc");
                assert_eq!(model, "codex");
            }
            _ => panic!("Expected SessionInit"),
        }
    }

    #[test]
    fn test_parse_codex_agent_message() {
        let line = r#"{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"I checked the run."}}"#;
        let events = parse_line(line);
        assert_eq!(events.len(), 1);
        match &events[0] {
            Event::Text { text } => assert_eq!(text, "I checked the run."),
            _ => panic!("Expected Text"),
        }
    }

    #[test]
    fn test_parse_codex_command_started() {
        let line = r#"{"type":"item.started","item":{"id":"item_1","type":"command_execution","command":"/bin/zsh -lc pwd","aggregated_output":"","exit_code":null,"status":"in_progress"}}"#;
        let events = parse_line(line);
        assert_eq!(events.len(), 1);
        match &events[0] {
            Event::ToolUse { name, summary, .. } => {
                assert_eq!(name, "Bash");
                assert!(summary.contains("/bin/zsh -lc pwd"));
            }
            _ => panic!("Expected ToolUse"),
        }
    }

    #[test]
    fn test_parse_codex_command_completed_output() {
        let line = r#"{"type":"item.completed","item":{"id":"item_1","type":"command_execution","command":"/bin/zsh -lc pwd","aggregated_output":"/tmp/project\n","exit_code":0,"status":"completed"}}"#;
        let events = parse_line(line);
        assert_eq!(events.len(), 1);
        match &events[0] {
            Event::ToolResult { content, .. } => {
                assert_eq!(content, "/tmp/project\n");
            }
            _ => panic!("Expected ToolResult"),
        }
    }

    #[test]
    fn test_parse_codex_turn_events() {
        assert!(parse_line(r#"{"type":"turn.started"}"#).is_empty());

        let events = parse_line(r#"{"type":"turn.completed"}"#);
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], Event::Result { .. }));
    }

    #[test]
    fn test_parse_system_non_init() {
        let line = r#"{"type":"system","subtype":"config"}"#;
        let events = parse_line(line);
        assert_eq!(events.len(), 1);
        match &events[0] {
            Event::Unknown { event_type } => {
                assert_eq!(event_type, "system:config");
            }
            _ => panic!("Expected Unknown"),
        }
    }

    #[test]
    fn test_parse_assistant_empty_content() {
        let line = r#"{"type":"assistant","message":{"content":[]}}"#;
        let events = parse_line(line);
        assert!(events.is_empty());
    }

    #[test]
    fn test_unknown_event_summary() {
        let event = Event::Unknown {
            event_type: "mystery".to_string(),
        };
        assert_eq!(event.summary(), "(mystery)");
    }

    #[test]
    fn test_incremental_reader_initial_read() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("transcript.jsonl");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, r#"{{"type":"rate_limit_event"}}"#).unwrap();
        }

        let mut reader = TranscriptReader::new(path);
        let events = reader.read_new();
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], Event::RateLimit));
    }

    #[test]
    fn test_incremental_reader_idempotent() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("transcript.jsonl");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, r#"{{"type":"rate_limit_event"}}"#).unwrap();
        }

        let mut reader = TranscriptReader::new(path);
        reader.read_new();
        let events = reader.read_new();
        assert!(events.is_empty());
    }

    #[test]
    fn test_incremental_reader_appended_data() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("transcript.jsonl");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, r#"{{"type":"rate_limit_event"}}"#).unwrap();
        }

        let mut reader = TranscriptReader::new(path.clone());
        reader.read_new();

        {
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            writeln!(
                f,
                r#"{{"type":"assistant","message":{{"content":[{{"type":"text","text":"hello"}}]}}}}"#
            )
            .unwrap();
        }

        let events = reader.read_new();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], Event::Text { text } if text == "hello"));
    }

    #[test]
    fn test_list_transcripts_ordered() {
        let tmp = TempDir::new().unwrap();
        let sessions = tmp.path().join("sessions");

        for n in [3, 1, 2] {
            let dir = sessions.join(format!("session-{n}"));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("transcript.jsonl"), "").unwrap();
        }

        let results = list_transcripts(tmp.path());
        let nums: Vec<u32> = results.iter().map(|(n, _)| *n).collect();
        assert_eq!(nums, vec![1, 2, 3]);
    }

    #[test]
    fn test_find_latest_transcript() {
        let tmp = TempDir::new().unwrap();
        let sessions = tmp.path().join("sessions");

        for n in [1, 2, 3] {
            let dir = sessions.join(format!("session-{n}"));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("transcript.jsonl"), "").unwrap();
        }

        let latest = find_latest_transcript(tmp.path()).unwrap();
        assert!(latest.to_string_lossy().contains("session-3"));
    }
}
