//! Fail-closed provider outage evidence parsed from canonical coder transcripts.

use crate::coder::CoderKind;
use serde_json::Value;
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
/// transcript contain an explicit provider rate-limit event and no model text or
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
    let mut unavailable = false;
    for line in BufReader::new(file).lines() {
        let Ok(line) = line else {
            return false;
        };
        let Ok(event) = serde_json::from_str::<Value>(&line) else {
            return false;
        };
        if model_progressed(provider, &event) {
            return false;
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
    unavailable
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
        for phase in 0..crate::coder::RATE_LIMIT_MAX_RETRIES {
            std::fs::write(
                phase_transcript_path(&path, phase),
                "{\"type\":\"rate_limit_event\",\"retry_after\":1}\n",
            )
            .unwrap();
        }
        writeln!(file, r#"{{"type":"system"}}"#).unwrap();
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
}
