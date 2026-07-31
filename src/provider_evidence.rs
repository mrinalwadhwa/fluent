//! Fail-closed provider outage evidence parsed from canonical coder transcripts.

use crate::coder::CoderKind;
use serde_json::Value;
use std::fmt;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

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

/// Return evidence only for a complete JSONL transcript containing one explicit
/// provider rate-limit terminal event and no model text or tool activity.
/// Unknown providers and unsupported transcript shapes intentionally fail closed.
pub fn classify_provider_unavailable(
    provider: CoderKind,
    transcript: &Path,
) -> Option<ProviderUnavailable> {
    if !matches!(provider, CoderKind::Claude | CoderKind::Codex) {
        return None;
    }
    let file = File::open(transcript).ok()?;
    let mut unavailable = false;
    for line in BufReader::new(file).lines() {
        let line = line.ok()?;
        let event: Value = serde_json::from_str(&line).ok()?;
        if model_progressed(provider, &event) {
            return None;
        }
        unavailable |= match provider {
            CoderKind::Claude => {
                event["type"].as_str() == Some("rate_limit_event")
                    && (event["retry_after"].is_u64()
                        || event["retry_after_ms"].is_u64()
                        || event["reset_at"].is_string())
            }
            CoderKind::Codex => {
                event["type"].as_str() == Some("error")
                    && event["code"].as_str() == Some("rate_limit")
                    && (event["retry_after"].is_u64()
                        || event["retry_after_ms"].is_u64()
                        || event["reset_at"].is_string())
            }
            CoderKind::Pi => false,
        };
    }
    unavailable.then_some(ProviderUnavailable { provider })
}

fn model_progressed(provider: CoderKind, event: &Value) -> bool {
    match provider {
        CoderKind::Claude => {
            event["type"].as_str() == Some("assistant")
                && event["message"]["content"]
                    .as_array()
                    .is_some_and(|content| {
                        content.iter().any(|part| {
                            matches!(part["type"].as_str(), Some("text") | Some("tool_use"))
                        })
                    })
        }
        CoderKind::Codex => matches!(
            event["item"]["type"].as_str(),
            Some("agent_message")
                | Some("reasoning")
                | Some("command_execution")
                | Some("function_call")
        ),
        CoderKind::Pi => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn classifies_structured_unstarted_claude_outage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let mut file = File::create(&path).unwrap();
        writeln!(file, r#"{{"type":"system"}}"#).unwrap();
        writeln!(file, r#"{{"type":"rate_limit_event","retry_after":1}}"#).unwrap();
        assert!(classify_provider_unavailable(CoderKind::Claude, &path).is_some());
    }

    #[test]
    fn rejects_started_or_malformed_transcripts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        std::fs::write(&path, "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"hi\"}]}}\n{\"type\":\"rate_limit_event\",\"retry_after\":1}\n").unwrap();
        assert!(classify_provider_unavailable(CoderKind::Claude, &path).is_none());
        std::fs::write(&path, "not json\n").unwrap();
        assert!(classify_provider_unavailable(CoderKind::Claude, &path).is_none());
    }
}
