//! Fail-closed provider outage evidence parsed from canonical coder transcripts.

use crate::coder::CoderKind;
use serde_json::{Map, Value};
use std::fmt;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

/// Evidence that a provider rejected a launch before the model made progress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderUnavailable {
    pub provider: CoderKind,
}

/// The typed terminal error used to keep an exhausted provider outage out of
/// Fluent's generic Task retry loop.
#[derive(Debug, Clone)]
pub struct ProviderUnavailableError(pub ProviderUnavailable);

impl fmt::Display for ProviderUnavailableError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} provider unavailable before model progress",
            self.0.provider.as_str()
        )
    }
}

impl std::error::Error for ProviderUnavailableError {}

/// Return evidence only after every bounded provider retry phase and the terminal
/// transcript contain an explicit provider rate-limit event and no model tokens or
/// tool activity. Unknown providers and unsupported transcript shapes fail closed.
pub fn classify_provider_unavailable(
    provider: CoderKind,
    transcript: &Path,
) -> Option<ProviderUnavailable> {
    if !matches!(provider, CoderKind::Claude | CoderKind::Codex) {
        return None;
    }
    let phase_paths = preserved_phase_paths(transcript)?;
    if phase_paths.len() < crate::coder::RATE_LIMIT_MAX_RETRIES as usize {
        return None;
    }
    for phase_path in phase_paths {
        if !classify_provider_phase(provider, &phase_path) {
            return None;
        }
    }
    classify_provider_phase(provider, transcript).then_some(ProviderUnavailable { provider })
}

/// Recognize the supported final native Codex quota stream. Earlier launches
/// may have made progress; this proves only this captured terminal failure.
/// Session rollouts and model-authored quota text never enter this grammar.
pub(crate) fn codex_usage_limit_failure(bytes: &[u8]) -> Option<String> {
    let events = parse_json_lines(std::str::from_utf8(bytes).ok()?)?;
    let [thread, turn, error, failed] = events.as_slice() else {
        return None;
    };
    let thread_id = thread["thread_id"].as_str()?.to_string();
    let message = error["message"].as_str()?;
    let objects_have_supported_keys = [
        (thread, &["type", "thread_id"][..]),
        (turn, &["type"][..]),
        (error, &["type", "message"][..]),
        (failed, &["type", "error"][..]),
        (&failed["error"], &["message"][..]),
    ]
    .iter()
    .all(|(value, keys)| {
        value.as_object().is_some_and(|object| {
            object.len() == keys.len() && object.keys().all(|key| keys.contains(&key.as_str()))
        })
    });
    (objects_have_supported_keys
        && !thread_id.is_empty()
        && thread_id.len() <= 256
        && thread["type"] == "thread.started"
        && turn["type"] == "turn.started"
        && error["type"] == "error"
        && failed["type"] == "turn.failed"
        && message.starts_with("You've hit your usage limit. ")
        && message.len() <= 4096
        && !message.contains(['\n', '\r'])
        && failed["error"]["message"].as_str() == Some(message))
    .then_some(thread_id)
}

/// Recognize a Codex launch that exhausted its own reconnect path before the
/// model produced any message or tool activity.
///
/// This classifier intentionally accepts only Codex's small structured
/// transport-failure grammar. Any unknown event or usable model progress makes
/// the evidence inconclusive and leaves the failure hard.
pub(crate) fn classify_codex_transport_unavailable(
    transcript: &Path,
) -> Option<ProviderUnavailable> {
    let events = read_json_lines(transcript)?;
    let mut saw_turn = false;
    let mut saw_transport_failure = false;
    let mut saw_terminal_failure = false;

    for (index, event) in events.iter().enumerate() {
        match event["type"].as_str()? {
            "thread.started" if !saw_turn && !saw_terminal_failure => {}
            "turn.started" if !saw_turn && !saw_terminal_failure => saw_turn = true,
            "error" if saw_turn && !saw_terminal_failure => {
                if !is_codex_transport_failure_message(event["message"].as_str()?) {
                    return None;
                }
                saw_transport_failure = true;
            }
            "item.completed" if saw_turn && !saw_terminal_failure => {
                if event["item"]["type"].as_str() != Some("error")
                    || !is_codex_transport_failure_message(event["item"]["message"].as_str()?)
                {
                    return None;
                }
                saw_transport_failure = true;
            }
            "turn.failed" if saw_turn && index + 1 == events.len() => {
                if !is_codex_transport_failure_message(event["error"]["message"].as_str()?) {
                    return None;
                }
                saw_transport_failure = true;
                saw_terminal_failure = true;
            }
            _ => return None,
        }
    }

    (saw_turn && saw_transport_failure && saw_terminal_failure).then_some(ProviderUnavailable {
        provider: CoderKind::Codex,
    })
}

/// Recognize a Codex launch that failed authentication before the model made
/// any progress. Older Fluent builds could flatten this failure to a generic
/// `RoundCap` task when the CLI emitted the auth error directly in its
/// structured transcript.  Recovery uses this narrow grammar to migrate only
/// that historical shape back to the normal resumable Auth pause.
pub(crate) fn classify_codex_auth_failure(transcript: &Path) -> bool {
    let Some(events) = read_json_lines(transcript) else {
        return false;
    };
    let mut saw_turn = false;
    let mut saw_auth_failure = false;
    let mut saw_terminal_failure = false;

    for (index, event) in events.iter().enumerate() {
        let Some(event_type) = event["type"].as_str() else {
            return false;
        };
        match event_type {
            "thread.started" if !saw_turn && !saw_terminal_failure => {}
            "turn.started" if !saw_turn && !saw_terminal_failure => saw_turn = true,
            "error" if saw_turn && !saw_terminal_failure => {
                if !is_codex_auth_failure_message(event["message"].as_str().unwrap_or_default()) {
                    return false;
                }
                saw_auth_failure = true;
            }
            "item.completed" if saw_turn && !saw_terminal_failure => {
                if event["item"]["type"].as_str() != Some("error")
                    || !is_codex_auth_failure_message(
                        event["item"]["message"].as_str().unwrap_or_default(),
                    )
                {
                    return false;
                }
                saw_auth_failure = true;
            }
            "turn.failed" if saw_turn && index + 1 == events.len() => {
                if !is_codex_auth_failure_message(
                    event["error"]["message"].as_str().unwrap_or_default(),
                ) {
                    return false;
                }
                saw_auth_failure = true;
                saw_terminal_failure = true;
            }
            _ => return false,
        }
    }

    saw_turn && saw_auth_failure && saw_terminal_failure
}

fn is_codex_auth_failure_message(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    (message.contains("access token")
        && (message.contains("refresh")
            || message.contains("expired")
            || message.contains("authentication")))
        || message.contains("refresh_token_reused")
        || (message.contains("authentication token")
            && (message.contains("expired") || message.contains("invalid")))
        || ((message.contains("sign in") || message.contains("log out"))
            && (message.contains("token") || message.contains("authentication")))
}

fn is_codex_transport_failure_message(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    message.contains("stream disconnected before completion")
        && (message.contains("connection refused")
            || message.contains("error sending request for url"))
}

/// Recognize only the preserved single-file Claude scheduler outage grammar.
///
/// This compatibility classifier is intentionally separate from current Task
/// classification. It exists only so the Attempt loop can migrate historical
/// review failures whose transcripts predate numbered provider evidence.
pub(crate) fn classify_legacy_claude_scheduler_outage(
    transcript: &Path,
) -> Option<ProviderUnavailable> {
    let actual = read_json_lines(transcript)?;
    let expected = parse_json_lines(include_str!(
        "../tests/fixtures/provider-transcripts/claude-scheduler-outage-structural-manifest.jsonl"
    ))?;
    if actual.len() != expected.len() || actual.len() != 13 {
        return None;
    }

    let session_id = actual
        .first()?
        .get("session_id")?
        .as_str()
        .filter(|value| !value.is_empty())?
        .to_string();
    let normalized = actual
        .into_iter()
        .enumerate()
        .map(|(index, event)| normalize_legacy_scheduler_event(event, index, &session_id))
        .collect::<Option<Vec<_>>>()?;

    (normalized == expected).then_some(ProviderUnavailable {
        provider: CoderKind::Claude,
    })
}

fn read_json_lines(path: &Path) -> Option<Vec<Value>> {
    let file = File::open(path).ok()?;
    BufReader::new(file)
        .lines()
        .map(|line| serde_json::from_str(&line.ok()?).ok())
        .collect()
}

fn parse_json_lines(content: &str) -> Option<Vec<Value>> {
    content
        .lines()
        .map(|line| serde_json::from_str(line).ok())
        .collect()
}

fn normalize_legacy_scheduler_event(
    mut event: Value,
    index: usize,
    session_id: &str,
) -> Option<Value> {
    let object = event.as_object_mut()?;
    match index {
        0 => {
            remove_required_string(object, "cwd")?;
            require_and_remove_session(object, session_id)?;
            remove_required_string(object, "uuid")?;
            let plugins = object.get_mut("plugins")?.as_array_mut()?;
            for plugin in plugins {
                let plugin = plugin.as_object_mut()?;
                let name = plugin.get("name")?.as_str()?.to_string();
                remove_required_string(plugin, "path")?;
                plugin.insert(
                    "path".to_string(),
                    Value::String(format!("/normalized/plugins/{name}")),
                );
            }
        }
        1..=10 => {
            remove_required_u64(object, "retry_delay_ms")?;
            require_and_remove_session(object, session_id)?;
            remove_required_string(object, "uuid")?;
        }
        11 => {
            require_and_remove_session(object, session_id)?;
            remove_required_string(object, "uuid")?;
            let message = object.get_mut("message")?.as_object_mut()?;
            remove_required_string(message, "id")?;
        }
        12 => {
            remove_required_u64(object, "duration_ms")?;
            if remove_required_u64(object, "duration_api_ms")? != 0 {
                return None;
            }
            require_and_remove_session(object, session_id)?;
            remove_required_string(object, "uuid")?;
        }
        _ => return None,
    }
    Some(event)
}

fn require_and_remove_session(object: &mut Map<String, Value>, expected: &str) -> Option<()> {
    (remove_required_string(object, "session_id")? == expected).then_some(())
}

fn remove_required_string(object: &mut Map<String, Value>, key: &str) -> Option<String> {
    object
        .remove(key)?
        .as_str()
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn remove_required_u64(object: &mut Map<String, Value>, key: &str) -> Option<u64> {
    object.remove(key)?.as_u64()
}

/// List every immutable retry transcript alongside the live transcript. A later
/// generic retry preserves additional phases, and all of them are evidence: one
/// missing, malformed, or progressed phase must prevent the typed pause.
fn preserved_phase_paths(transcript: &Path) -> Option<Vec<PathBuf>> {
    let parent = transcript.parent()?;
    let stem = transcript.file_stem()?.to_str()?;
    let extension = transcript.extension()?.to_str()?;
    let prefix = format!("{stem}.");
    let suffix = format!(".{extension}");
    let mut phases = Vec::new();

    for entry in std::fs::read_dir(parent).ok()? {
        let path = entry.ok()?.path();
        let name = path.file_name()?.to_str()?;
        let Some(number) = name
            .strip_prefix(&prefix)
            .and_then(|name| name.strip_suffix(&suffix))
        else {
            continue;
        };
        let phase = number.parse::<u32>().ok()?;
        phases.push((phase, path));
    }
    phases.sort_by_key(|(phase, _)| *phase);
    if phases
        .iter()
        .enumerate()
        .any(|(expected, (actual, _))| *actual != expected as u32)
    {
        return None;
    }
    Some(phases.into_iter().map(|(_, path)| path).collect())
}

#[cfg(test)]
fn phase_transcript_path(transcript: &Path, phase: u32) -> PathBuf {
    let mut name = transcript
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_string())
        .unwrap_or_default();
    name.push_str(&format!(".{phase}"));
    if let Some(extension) = transcript.extension() {
        name.push('.');
        name.push_str(&extension.to_string_lossy());
    }
    transcript.with_file_name(name)
}

fn classify_provider_phase(provider: CoderKind, transcript: &Path) -> bool {
    let Ok(file) = File::open(transcript) else {
        return false;
    };
    let mut terminal_rate_limit = false;
    for line in BufReader::new(file).lines() {
        let Ok(line) = line else {
            return false;
        };
        let Ok(event) = serde_json::from_str::<Value>(&line) else {
            return false;
        };
        if terminal_rate_limit {
            return false;
        }
        if is_terminal_rate_limit(provider, &event) {
            terminal_rate_limit = true;
        } else if !is_safe_prelude(provider, &event) {
            return false;
        }
    }
    terminal_rate_limit
}

/// Accept only provider launch metadata before the terminal structured
/// rate-limit event. Any assistant, item, unknown, or post-terminal record
/// fails closed, including Claude thinking blocks that produced model tokens.
fn is_safe_prelude(provider: CoderKind, event: &Value) -> bool {
    match provider {
        CoderKind::Claude => {
            event["type"].as_str() == Some("system") && event["subtype"].as_str() == Some("init")
        }
        CoderKind::Codex => matches!(
            event["type"].as_str(),
            Some("thread.started") | Some("turn.started")
        ),
        CoderKind::Opencode => false,
        CoderKind::Pi => false,
    }
}

fn is_terminal_rate_limit(provider: CoderKind, event: &Value) -> bool {
    let has_timing = event["retry_after"].is_u64()
        || event["retry_after_ms"].is_u64()
        || event["reset_at"].is_string();
    match provider {
        CoderKind::Claude => event["type"].as_str() == Some("rate_limit_event") && has_timing,
        CoderKind::Codex => {
            event["type"].as_str() == Some("error")
                && event["code"].as_str() == Some("rate_limit")
                && has_timing
        }
        CoderKind::Opencode => false,
        CoderKind::Pi => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn legacy_scheduler_fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/provider-transcripts")
            .join(name)
    }

    #[test]
    fn opencode_has_no_prelude_or_rate_limit_evidence() {
        let directory = tempfile::tempdir().unwrap();
        let transcript = directory.path().join("transcript.jsonl");
        std::fs::write(
            &transcript,
            "{\"type\":\"step_start\",\"sessionID\":\"ses_1\"}\n{\"type\":\"error\",\"error\":{\"data\":{\"statusCode\":429}}}\n",
        )
        .unwrap();

        assert!(classify_provider_unavailable(CoderKind::Opencode, &transcript).is_none());
        assert!(!is_safe_prelude(
            CoderKind::Opencode,
            &serde_json::json!({"type":"step_start"})
        ));
        assert!(!is_terminal_rate_limit(
            CoderKind::Opencode,
            &serde_json::json!({"type":"error","retry_after":1})
        ));
    }

    #[test]
    fn usage_limit_requires_matching_native_terminal_events() {
        let transcript = concat!(
            "{\"type\":\"thread.started\",\"thread_id\":\"saved-thread\"}\n",
            "{\"type\":\"turn.started\"}\n",
            "{\"type\":\"error\",\"message\":\"You've hit your usage limit. Try again later.\"}\n",
            "{\"type\":\"turn.failed\",\"error\":{\"message\":\"You've hit your usage limit. Try again later.\"}}\n",
        );
        assert_eq!(
            codex_usage_limit_failure(transcript.as_bytes()).as_deref(),
            Some("saved-thread")
        );
        for invalid in [
            transcript.replace("turn.failed", "turn.completed"),
            transcript.replacen("Try again later.", "Connection failed.", 1),
            transcript.replace("\"error\",\"message\"", "\"agent_message\",\"message\""),
            format!("{transcript}{{}}\n"),
            transcript.lines().skip(1).collect::<Vec<_>>().join("\n"),
        ] {
            assert!(
                codex_usage_limit_failure(invalid.as_bytes()).is_none(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn accepts_exact_legacy_scheduler_outage_transcripts() {
        for name in [
            "claude-scheduler-outage-a.jsonl",
            "claude-scheduler-outage-b.jsonl",
        ] {
            assert_eq!(
                classify_legacy_claude_scheduler_outage(&legacy_scheduler_fixture(name)),
                Some(ProviderUnavailable {
                    provider: CoderKind::Claude,
                }),
                "the normalized preserved transcript {name} must match"
            );
        }
    }

    #[test]
    fn classifies_structured_unstarted_claude_outage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let mut file = File::create(&path).unwrap();
        for phase in 0..crate::coder::RATE_LIMIT_MAX_RETRIES {
            std::fs::write(
                phase_transcript_path(&path, phase),
                "{\"type\":\"rate_limit_event\",\"retry_after\":1}\n",
            )
            .unwrap();
        }
        writeln!(file, r#"{{"type":"system","subtype":"init"}}"#).unwrap();
        writeln!(file, r#"{{"type":"rate_limit_event","retry_after":1}}"#).unwrap();
        assert!(classify_provider_unavailable(CoderKind::Claude, &path).is_some());
    }

    #[test]
    fn rejects_started_or_malformed_transcripts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        for phase in 0..crate::coder::RATE_LIMIT_MAX_RETRIES {
            std::fs::write(
                phase_transcript_path(&path, phase),
                "{\"type\":\"rate_limit_event\",\"retry_after\":1}\n",
            )
            .unwrap();
        }
        std::fs::write(&path, "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"hi\"}]}}\n{\"type\":\"rate_limit_event\",\"retry_after\":1}\n").unwrap();
        assert!(classify_provider_unavailable(CoderKind::Claude, &path).is_none());
        std::fs::write(&path, "not json\n").unwrap();
        assert!(classify_provider_unavailable(CoderKind::Claude, &path).is_none());
    }

    #[test]
    fn rejects_nonterminal_rate_limit_evidence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        for phase in 0..crate::coder::RATE_LIMIT_MAX_RETRIES {
            std::fs::write(
                phase_transcript_path(&path, phase),
                "{\"type\":\"rate_limit_event\",\"retry_after\":1}\n",
            )
            .unwrap();
        }
        std::fs::write(
            &path,
            "{\"type\":\"rate_limit_event\",\"retry_after\":1}\n{\"type\":\"system\",\"subtype\":\"init\"}\n",
        )
        .unwrap();

        assert!(classify_provider_unavailable(CoderKind::Claude, &path).is_none());
    }

    #[test]
    fn rejects_claude_thinking_before_rate_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        for phase in 0..crate::coder::RATE_LIMIT_MAX_RETRIES {
            std::fs::write(
                phase_transcript_path(&path, phase),
                "{\"type\":\"rate_limit_event\",\"retry_after\":1}\n",
            )
            .unwrap();
        }
        std::fs::write(
            &path,
            "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"thinking\",\"thinking\":\"considering\"}]}}\n{\"type\":\"rate_limit_event\",\"retry_after\":1}\n",
        )
        .unwrap();

        assert!(classify_provider_unavailable(CoderKind::Claude, &path).is_none());
    }

    #[test]
    fn requires_every_provider_retry_phase() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        std::fs::write(
            &path,
            "{\"type\":\"error\",\"code\":\"rate_limit\",\"retry_after\":1}\n",
        )
        .unwrap();
        std::fs::write(
            phase_transcript_path(&path, 0),
            "{\"type\":\"error\",\"code\":\"rate_limit\",\"retry_after\":1}\n",
        )
        .unwrap();
        assert!(classify_provider_unavailable(CoderKind::Codex, &path).is_none());
    }

    #[test]
    fn rejects_progress_in_any_preserved_retry_phase() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        for phase in 0..crate::coder::RATE_LIMIT_MAX_RETRIES {
            std::fs::write(
                phase_transcript_path(&path, phase),
                "{\"type\":\"rate_limit_event\",\"retry_after\":1}\n",
            )
            .unwrap();
        }
        std::fs::write(
            phase_transcript_path(&path, crate::coder::RATE_LIMIT_MAX_RETRIES),
            "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"started\"}]}}\n",
        )
        .unwrap();
        std::fs::write(&path, "{\"type\":\"rate_limit_event\",\"retry_after\":1}\n").unwrap();

        assert!(classify_provider_unavailable(CoderKind::Claude, &path).is_none());
    }

    #[test]
    fn classifies_codex_transport_outage_before_progress() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        std::fs::write(
            &path,
            concat!(
                "{\"type\":\"thread.started\"}\n",
                "{\"type\":\"turn.started\"}\n",
                "{\"type\":\"error\",\"message\":\"Reconnecting... stream disconnected before completion: Connection refused (os error 61)\"}\n",
                "{\"type\":\"item.completed\",\"item\":{\"type\":\"error\",\"message\":\"Falling back. stream disconnected before completion: error sending request for url (https://example.invalid)\"}}\n",
                "{\"type\":\"turn.failed\",\"error\":{\"message\":\"stream disconnected before completion: error sending request for url (https://example.invalid)\"}}\n",
            ),
        )
        .unwrap();

        assert_eq!(
            classify_codex_transport_unavailable(&path),
            Some(ProviderUnavailable {
                provider: CoderKind::Codex,
            })
        );
    }

    #[test]
    fn classifies_codex_auth_failure_before_progress() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        std::fs::write(
            &path,
            concat!(
                "{\"type\":\"thread.started\"}\n",
                "{\"type\":\"turn.started\"}\n",
                "{\"type\":\"error\",\"message\":\"Your access token could not be refreshed because your refresh token was already used. Please log out and sign in again.\"}\n",
                "{\"type\":\"turn.failed\",\"error\":{\"message\":\"Your access token could not be refreshed because your refresh token was already used. Please log out and sign in again.\"}}\n",
            ),
        )
        .unwrap();

        assert!(classify_codex_auth_failure(&path));
    }

    #[test]
    fn rejects_codex_auth_failure_after_model_progress() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        std::fs::write(
            &path,
            concat!(
                "{\"type\":\"thread.started\"}\n",
                "{\"type\":\"turn.started\"}\n",
                "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"working\"}}\n",
                "{\"type\":\"turn.failed\",\"error\":{\"message\":\"Your access token could not be refreshed. Please sign in again.\"}}\n",
            ),
        )
        .unwrap();

        assert!(!classify_codex_auth_failure(&path));
    }

    #[test]
    fn rejects_codex_transport_outage_after_model_progress() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        std::fs::write(
            &path,
            concat!(
                "{\"type\":\"thread.started\"}\n",
                "{\"type\":\"turn.started\"}\n",
                "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"working\"}}\n",
                "{\"type\":\"turn.failed\",\"error\":{\"message\":\"stream disconnected before completion: Connection refused (os error 61)\"}}\n",
            ),
        )
        .unwrap();

        assert!(classify_codex_transport_unavailable(&path).is_none());
    }
}
