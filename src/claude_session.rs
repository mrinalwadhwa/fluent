//! Inspect Claude conversation identity without interpreting its private history.

use anyhow::{Context, Result};
use serde_json::Value;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::path::Path;

pub(crate) const FALLBACK_FILE: &str = "claude-session-fallback.json";

thread_local! {
    static FALLBACK_LAUNCH: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Limit the second-spawn exception to the proven rejection's fresh launch.
/// The execution boundary still checks cancellation and consumes a durable
/// marker under its model lock; this scope alone grants no retry authority.
pub(crate) struct FallbackLaunch(bool);
impl FallbackLaunch {
    pub(crate) fn enter() -> Self {
        Self(FALLBACK_LAUNCH.with(|flag| flag.replace(true)))
    }
    pub(crate) fn active() -> bool {
        FALLBACK_LAUNCH.with(|flag| flag.get())
    }
}
impl Drop for FallbackLaunch {
    fn drop(&mut self) {
        FALLBACK_LAUNCH.with(|flag| flag.set(self.0));
    }
}

/// The native adapter constructs this only after an exact pre-SessionStart
/// rejection and verified settlement of its managed terminal and descendants.
#[derive(Debug)]
pub(crate) struct MissingConversation(pub(crate) String);
impl std::fmt::Display for MissingConversation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "Claude rejected missing conversation {} before SessionStart",
            self.0
        )
    }
}
impl std::error::Error for MissingConversation {}

pub(crate) fn run_native_with_fallback<'a>(
    session: crate::coder::CoderSession<'a>,
    eligible: bool,
    already_fresh: bool,
    transcript: &Path,
    mut launch: impl FnMut(crate::coder::CoderSession<'a>) -> Result<()>,
) -> Result<()> {
    let result = launch(session);
    if already_fresh {
        return result.context(FallbackStopped);
    }
    if eligible
        && let crate::coder::CoderSession::Resume(id) = session
        && result
            .as_ref()
            .err()
            .and_then(|e| e.downcast_ref::<MissingConversation>())
            .is_some_and(|rejection| rejection.0 == id)
    {
        reserve_fallback(
            transcript
                .parent()
                .context("native Claude has no evidence area")?,
            id,
            "provider rejected the exact native resume target before SessionStart",
        )?;
        crate::coder::preserve_claude_invocation(transcript).context(FallbackStopped)?;
        let _fallback = FallbackLaunch::enter();
        return launch(crate::coder::CoderSession::FreshPersistent).context(FallbackStopped);
    }
    result
}

/// Keep eligibility across generic retries and handoff correction. Once an
/// invocation starts, a later invocation cannot prove that this Task never
/// progressed, even if the provider replaces or removes its local history.
pub(crate) fn begin_invocation(transcript: &Path) -> Result<bool> {
    let marker = transcript.with_file_name("claude-session-invoked");
    let first = match OpenOptions::new().write(true).create_new(true).open(marker) {
        Ok(file) => {
            file.sync_all()?;
            !transcript.try_exists()?
        }
        Err(error) if error.kind() == ErrorKind::AlreadyExists => false,
        Err(error) => return Err(error.into()),
    };
    if !first {
        crate::coder::preserve_claude_invocation(transcript)?;
    }
    Ok(first)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StoredSession {
    Available,
    Missing,
    Uncertain,
}

pub(crate) fn valid_id(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(index, byte)| {
            if [8, 13, 18, 23].contains(&index) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

/// Only conclude absence after inspecting regular project directories. An
/// unreadable directory, symlink, or unfamiliar transcript stays uncertain.
pub(crate) fn stored_session(home: &Path, id: &str) -> Result<StoredSession> {
    if !valid_id(id) {
        return Ok(StoredSession::Uncertain);
    }
    let projects = home.join("projects");
    match fs::symlink_metadata(&projects) {
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(StoredSession::Missing),
        Err(error) => return Err(error.into()),
        Ok(metadata) if !metadata.is_dir() => return Ok(StoredSession::Uncertain),
        Ok(_) => {}
    }
    let mut uncertain = false;
    for entry in fs::read_dir(projects)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            uncertain |= entry.file_type()?.is_symlink();
            continue;
        }
        let path = entry.path().join(format!("{id}.jsonl"));
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
            Ok(metadata) if !metadata.is_file() => {
                uncertain = true;
                continue;
            }
            Ok(_) => {}
        }
        // Read only an identity-bearing prefix, never reconstruct Claude's
        // private conversation format. Large or unfamiliar prefixes resume
        // through the provider without being called absent.
        let mut bytes = Vec::new();
        File::open(path)?.take(64 * 1024).read_to_end(&mut bytes)?;
        for line in bytes.split(|byte| *byte == b'\n').take(32) {
            if let Ok(value) = serde_json::from_slice::<Value>(line)
                && value["sessionId"].as_str() == Some(id)
            {
                return Ok(StoredSession::Available);
            }
        }
        uncertain = true;
    }
    Ok(if uncertain {
        StoredSession::Uncertain
    } else {
        StoredSession::Missing
    })
}

/// Recognize the released Claude zero-turn rejection, not quoted diagnostics
/// in assistant output or a generic provider failure that mentions a session.
pub(crate) fn is_missing_conversation(transcript: &[u8], stderr: &[u8], id: &str) -> bool {
    if !valid_id(id) || transcript.len() > 64 * 1024 || stderr.len() > 4096 {
        return false;
    }
    let Ok(value) = serde_json::from_slice::<Value>(transcript) else {
        return false;
    };
    let expected = format!("No conversation found with session ID: {id}");
    let Ok(stderr) = std::str::from_utf8(stderr) else {
        return false;
    };
    if !stderr.trim().is_empty() && stderr.trim() != expected {
        return false;
    }
    let Some(object) = value.as_object() else {
        return false;
    };
    if object.keys().any(|key| {
        !matches!(
            key.as_str(),
            "type"
                | "subtype"
                | "is_error"
                | "num_turns"
                | "session_id"
                | "errors"
                | "duration_ms"
                | "duration_api_ms"
                | "total_cost_usd"
                | "usage"
                | "modelUsage"
                | "permission_denials"
                | "uuid"
                | "fast_mode_state"
                | "result"
                | "stop_reason"
                | "result_index"
        )
    }) {
        return false;
    }
    value["type"] == "result"
        && value["subtype"] == "error_during_execution"
        && value["is_error"] == true
        && value["num_turns"].as_u64() == Some(0)
        && value["session_id"].as_str() == Some(id)
        && value["errors"] == serde_json::json!([expected])
        && ["duration_api_ms", "total_cost_usd"]
            .iter()
            .all(|key| value.get(key).is_none_or(|v| v.as_f64() == Some(0.0)))
        // Claude 2.1.307 emits these even when it rejects resume before a turn.
        // Admit only the observed values, never arbitrary null metadata.
        && value.get("stop_reason").is_none_or(Value::is_null)
        && value.get("result_index").is_none_or(|v| v.as_u64() == Some(0))
        && value.get("usage").is_none_or(zero_usage)
        && value
            .get("modelUsage")
            .is_none_or(|v| v.as_object().is_some_and(|o| o.is_empty()))
        && value
            .get("permission_denials")
            .is_none_or(|v| v.as_array().is_some_and(|a| a.is_empty()))
        && value.get("result").is_none_or(|v| v.as_str() == Some(""))
}

fn zero_usage(value: &Value) -> bool {
    let zero_counters = |value: &Value, keys: &[&str]| {
        value.as_object().is_some_and(|object| {
            object
                .iter()
                .all(|(key, value)| keys.contains(&key.as_str()) && value.as_u64() == Some(0))
        })
    };
    value.as_object().is_some_and(|object| {
        object.iter().all(|(key, value)| match key.as_str() {
            "input_tokens"
            | "output_tokens"
            | "cache_creation_input_tokens"
            | "cache_read_input_tokens" => value.as_u64() == Some(0),
            "server_tool_use" => {
                zero_counters(value, &["web_search_requests", "web_fetch_requests"])
            }
            "output_tokens_details" => zero_counters(value, &["thinking_tokens"]),
            "cache_creation" => zero_counters(
                value,
                &["ephemeral_1h_input_tokens", "ephemeral_5m_input_tokens"],
            ),
            "iterations" => value.as_array().is_some_and(|array| array.is_empty()),
            "service_tier" | "inference_geo" | "speed" => value.is_string(),
            "fallback_credit" => value.is_null(),
            _ => false,
        })
    })
}

pub(crate) fn settled_failure(completion: &crate::coder::CoderRunCompletion) -> bool {
    matches!(completion.terminal, Ok(code) if code != 0)
        && completion.report.launches.len() == 1
        && completion.report.launches.iter().all(|launch| {
            launch.exit_code.is_some_and(|code| code != 0)
                && launch.group_sweep.is_verified_swept()
                && launch.provider_liveness.is_none()
        })
}

/// A consumed fresh launch cannot enter the generic coder retry budget. Keep
/// this as an anyhow context so underlying auth and transport errors retain
/// their existing typed pause classifications.
#[derive(Debug)]
pub(crate) struct FallbackStopped;

impl std::fmt::Display for FallbackStopped {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Claude session fallback cannot be repeated for this Task")
    }
}
impl std::error::Error for FallbackStopped {}

pub(crate) fn reserve_fallback(area: &Path, id: &str, reason: &str) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(&serde_json::json!({
        "schema_version":1, "missing_session_id":id, "reason":reason,
    }))?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(area.join(FALLBACK_FILE))
        .context(FallbackStopped)?;
    file.write_all(&bytes).context(FallbackStopped)?;
    file.sync_all().context(FallbackStopped)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ID: &str = "11111111-1111-4111-8111-111111111111";

    #[test]
    fn invocation_preservation_failure_retains_transport_diagnostic() {
        for obstruction in ["transcript.jsonl", "transcript.0.jsonl"] {
            let dir = tempfile::tempdir().unwrap();
            let transcript = dir.path().join("transcript.jsonl");
            let status = crate::transcript_pump::status_path_for(&transcript);
            fs::write(&status, "previous pump diagnostic").unwrap();
            if obstruction != "transcript.jsonl" {
                fs::write(&transcript, "previous invocation").unwrap();
            }
            fs::create_dir(dir.path().join(obstruction)).unwrap();

            let error = begin_invocation(&transcript)
                .context(FallbackStopped)
                .unwrap_err();
            assert!(error.is::<FallbackStopped>());
            let pump = error
                .downcast_ref::<crate::transcript_pump::TranscriptPumpError>()
                .expect("preservation must retain the typed transport failure");
            assert!(pump.message().contains(obstruction), "{error:#}");
            let diagnostic: crate::transcript_pump::PumpStatus =
                serde_json::from_slice(&fs::read(&status).unwrap()).unwrap();
            assert_eq!(diagnostic.state, crate::transcript_pump::PumpState::Failed);
            assert_eq!(diagnostic.error.as_deref(), Some(pump.message()));
            assert_eq!(diagnostic.bytes, 0, "no provider was launched");
            assert!(diagnostic.transport.is_balanced());
            assert_eq!(
                fs::read_to_string(dir.path().join("transcript-pump.0.json")).unwrap(),
                "previous pump diagnostic"
            );
            let failed_status = fs::read(&status).unwrap();

            fs::remove_dir(dir.path().join(obstruction)).unwrap();
            assert!(
                !begin_invocation(&transcript).unwrap(),
                "repair must not restore fresh fallback eligibility"
            );
            assert_eq!(
                fs::read(dir.path().join("transcript-pump.1.json")).unwrap(),
                failed_status
            );
            if obstruction != "transcript.jsonl" {
                assert_eq!(
                    fs::read_to_string(dir.path().join("transcript.1.jsonl")).unwrap(),
                    "previous invocation"
                );
            }
            assert!(!dir.path().join(FALLBACK_FILE).exists());
        }
    }

    #[test]
    fn invocation_preservation_retries_after_partial_setup() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        assert!(begin_invocation(&transcript).unwrap());
        let stderr = crate::coder::provider_stderr_path(&transcript);
        fs::write(&stderr, "previous setup diagnostic").unwrap();
        let obstruction = dir.path().join("execution-receipt");
        fs::create_dir(&obstruction).unwrap();

        let error = begin_invocation(&transcript).unwrap_err();
        assert!(error.is::<crate::transcript_pump::TranscriptPumpError>());
        let preserved = stderr.with_extension("0.log");
        assert_eq!(
            fs::read_to_string(&preserved).unwrap(),
            "previous setup diagnostic"
        );

        fs::remove_dir(obstruction).unwrap();
        assert!(!begin_invocation(&transcript).unwrap());
        assert_eq!(
            fs::read_to_string(&preserved).unwrap(),
            "previous setup diagnostic"
        );
    }

    #[test]
    fn invocation_status_preservation_failure_keeps_original_evidence() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        let status = crate::transcript_pump::status_path_for(&transcript);
        fs::write(&transcript, "previous invocation").unwrap();
        fs::write(&status, "previous pump diagnostic").unwrap();
        let obstruction = dir.path().join("transcript-pump.0.json");
        fs::create_dir(&obstruction).unwrap();

        let error = begin_invocation(&transcript).unwrap_err();
        assert!(error.is::<crate::transcript_pump::TranscriptPumpError>());
        assert_eq!(
            fs::read_to_string(&status).unwrap(),
            "previous pump diagnostic"
        );
        assert_eq!(
            fs::read_to_string(&transcript).unwrap(),
            "previous invocation"
        );

        fs::remove_dir(obstruction).unwrap();
        assert!(!begin_invocation(&transcript).unwrap());
    }

    #[test]
    fn captured_claude_result_proves_no_progress() {
        let captured = include_bytes!("../tests/fixtures/claude-2.1.307-missing-conversation.json");
        let value: Value = serde_json::from_slice(captured).unwrap();
        let id = value["session_id"].as_str().unwrap();
        assert!(is_missing_conversation(captured, b"", id));
    }

    #[test]
    fn missing_session_requires_one_exact_zero_turn_result() {
        let result = json!({"type":"result", "subtype":"error_during_execution",
            "is_error":true,"num_turns":0,"session_id":ID,
            "errors":[format!("No conversation found with session ID: {ID}")]});
        assert!(is_missing_conversation(
            result.to_string().as_bytes(),
            b"",
            ID
        ));
        for field in ["num_turns", "session_id", "is_error"] {
            let mut invalid = result.clone();
            invalid.as_object_mut().unwrap().remove(field);
            assert!(!is_missing_conversation(
                invalid.to_string().as_bytes(),
                b"",
                ID
            ));
        }
        assert!(!is_missing_conversation(
            format!("{result}\n{result}").as_bytes(),
            b"",
            ID
        ));
        assert!(!is_missing_conversation(
            result.to_string().as_bytes(),
            b"authentication failed",
            ID
        ));
        let mut progress = result.clone();
        progress["usage"] = json!({"input_tokens":1});
        assert!(!is_missing_conversation(
            progress.to_string().as_bytes(),
            b"",
            ID
        ));
    }

    #[test]
    fn stored_session_distinguishes_absence_from_unreadable_or_malformed_state() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(
            stored_session(root.path(), ID).unwrap(),
            StoredSession::Missing
        );
        let project = root.path().join("projects/project");
        std::fs::create_dir_all(&project).unwrap();
        let path = project.join(format!("{ID}.jsonl"));
        std::fs::write(&path, b"not json\n").unwrap();
        assert_eq!(
            stored_session(root.path(), ID).unwrap(),
            StoredSession::Uncertain
        );
        std::fs::write(
            &path,
            format!("{{\"sessionId\":\"{ID}\",\"type\":\"user\"}}\n"),
        )
        .unwrap();
        assert_eq!(
            stored_session(root.path(), ID).unwrap(),
            StoredSession::Available
        );
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink("/missing", &path).unwrap();
        assert_eq!(
            stored_session(root.path(), ID).unwrap(),
            StoredSession::Uncertain
        );
        assert_eq!(
            stored_session(root.path(), "../../outside").unwrap(),
            StoredSession::Uncertain
        );
    }

    #[test]
    fn missing_session_requires_verified_process_settlement() {
        use crate::coder::{
            CoderLaunchSupervision, CoderRunCompletion, CoderSupervisionReport,
            GroupSweepDisposition,
        };
        for (sweep, accepted) in [
            (GroupSweepDisposition::AlreadyGone, true),
            (GroupSweepDisposition::Delivered, true),
            (GroupSweepDisposition::NotAttempted, false),
            (GroupSweepDisposition::IdentityLost, false),
        ] {
            let completion = CoderRunCompletion {
                terminal: Ok(1),
                report: CoderSupervisionReport {
                    launches: vec![CoderLaunchSupervision {
                        exit_code: Some(1),
                        group_sweep: sweep,
                        provider_liveness: None,
                    }],
                },
            };
            assert_eq!(settled_failure(&completion), accepted);
        }
        assert!(!settled_failure(&CoderRunCompletion {
            terminal: Ok(1),
            report: Default::default()
        }));
    }
}
