use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::Command;

use serde::Deserialize;

const KEYCHAIN_SERVICE: &str = "Claude Code-credentials";
const EXPIRY_MARGIN_MS: i64 = 5 * 60 * 1000;

#[derive(Debug, Clone)]
pub enum AuthError {
    Expired { expires_at: i64 },
    Rejected { request_id: Option<String> },
}

impl AuthError {
    pub fn user_message(&self) -> String {
        let prefix = match self {
            AuthError::Expired { .. } => "Claude auth token expired.",
            AuthError::Rejected { .. } => "Claude auth token rejected (HTTP 401).",
        };
        format!("{prefix} Run 'claude /login' to re-authenticate, then retry the Task.")
    }
}

#[derive(Deserialize)]
struct KeychainEnvelope {
    #[serde(rename = "claudeAiOauth")]
    claude_ai_oauth: Option<ClaudeAiOauth>,
}

#[derive(Deserialize)]
struct ClaudeAiOauth {
    #[serde(rename = "refreshToken")]
    refresh_token: Option<String>,
    #[serde(rename = "expiresAt")]
    expires_at: i64,
}

fn read_keychain() -> Option<ClaudeAiOauth> {
    let output = Command::new("security")
        .args(["find-generic-password", "-s", KEYCHAIN_SERVICE, "-w"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let s = String::from_utf8(output.stdout).ok()?;
    let s = s.trim();
    let envelope: KeychainEnvelope = serde_json::from_str(s).ok()?;
    envelope.claude_ai_oauth
}

/// Check that the Claude OAuth token has not expired (or is not about
/// to expire within 5 minutes). Returns `Ok(())` when no keychain
/// entry exists or the session has no refresh token, treating these as
/// API-key-only paths that skip the check.
pub fn ensure_not_expired() -> Result<(), AuthError> {
    let Some(creds) = read_keychain() else {
        return Ok(());
    };

    if creds.refresh_token.is_none() {
        return Ok(());
    }

    let now_ms = chrono::Utc::now().timestamp_millis();
    if creds.expires_at - now_ms > EXPIRY_MARGIN_MS {
        return Ok(());
    }

    Err(AuthError::Expired {
        expires_at: creds.expires_at,
    })
}

/// Walk a transcript JSONL file and return `AuthError::Rejected` if
/// the most recent `result` event has `api_error_status == 401`.
pub fn classify_transcript_401(transcript_path: &Path) -> Option<AuthError> {
    let file = File::open(transcript_path).ok()?;
    let reader = BufReader::new(file);

    let mut last: Option<AuthError> = None;
    for line in reader.lines().map_while(Result::ok) {
        let Ok(val) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if val.get("type").and_then(|v| v.as_str()) != Some("result") {
            continue;
        }
        let status = val.get("api_error_status").and_then(|v| v.as_i64());
        if status == Some(401) {
            let request_id = val
                .get("request_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            last = Some(AuthError::Rejected { request_id });
        } else {
            last = None;
        }
    }
    last
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn auth_error_expired_user_message_names_login_action() {
        let err = AuthError::Expired {
            expires_at: 1_700_000_000_000,
        };
        let msg = err.user_message();
        assert!(msg.contains("expired"), "should mention expired: {msg}");
        assert!(
            msg.contains("claude /login"),
            "should name recovery action: {msg}"
        );
        assert!(
            msg.contains("retry the Task"),
            "should mention retry: {msg}"
        );
    }

    #[test]
    fn auth_error_rejected_user_message_names_login_action() {
        let err = AuthError::Rejected {
            request_id: Some("req-123".into()),
        };
        let msg = err.user_message();
        assert!(
            msg.contains("rejected"),
            "should mention rejected: {msg}"
        );
        assert!(msg.contains("401"), "should mention 401: {msg}");
        assert!(
            msg.contains("claude /login"),
            "should name recovery action: {msg}"
        );
    }

    // -- classify_transcript_401 tests ---

    #[test]
    fn classify_transcript_401_returns_none_when_no_result_event() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let mut f = File::create(&path).unwrap();
        writeln!(f, r#"{{"type":"system","subtype":"init"}}"#).unwrap();
        writeln!(f, r#"{{"type":"assistant","content":"hello"}}"#).unwrap();
        drop(f);

        assert!(classify_transcript_401(&path).is_none());
    }

    #[test]
    fn classify_transcript_401_returns_rejected_on_result_401() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let mut f = File::create(&path).unwrap();
        writeln!(f, r#"{{"type":"system","subtype":"init"}}"#).unwrap();
        writeln!(
            f,
            r#"{{"type":"result","api_error_status":401,"request_id":"req-abc"}}"#
        )
        .unwrap();
        drop(f);

        let err = classify_transcript_401(&path).expect("should detect 401");
        assert!(matches!(err, AuthError::Rejected { .. }));
    }

    #[test]
    fn classify_transcript_401_returns_none_when_last_result_succeeded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let mut f = File::create(&path).unwrap();
        writeln!(
            f,
            r#"{{"type":"result","api_error_status":401,"request_id":"req-1"}}"#
        )
        .unwrap();
        writeln!(f, r#"{{"type":"result","api_error_status":0}}"#).unwrap();
        drop(f);

        assert!(classify_transcript_401(&path).is_none());
    }

    #[test]
    fn classify_transcript_401_extracts_request_id_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let mut f = File::create(&path).unwrap();
        writeln!(
            f,
            r#"{{"type":"result","api_error_status":401,"request_id":"req-xyz-789"}}"#
        )
        .unwrap();
        drop(f);

        let err = classify_transcript_401(&path).unwrap();
        match err {
            AuthError::Rejected { request_id } => {
                assert_eq!(request_id.as_deref(), Some("req-xyz-789"));
            }
            _ => panic!("expected Rejected variant"),
        }
    }

    #[test]
    fn classify_transcript_401_returns_rejected_with_none_request_id_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let mut f = File::create(&path).unwrap();
        writeln!(f, r#"{{"type":"result","api_error_status":401}}"#).unwrap();
        drop(f);

        let err = classify_transcript_401(&path).unwrap();
        match err {
            AuthError::Rejected { request_id } => {
                assert!(request_id.is_none());
            }
            _ => panic!("expected Rejected variant"),
        }
    }

    #[test]
    fn classify_transcript_401_returns_none_for_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.jsonl");
        assert!(classify_transcript_401(&path).is_none());
    }

    #[test]
    fn classify_transcript_401_returns_none_for_non_401_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let mut f = File::create(&path).unwrap();
        writeln!(f, r#"{{"type":"result","api_error_status":429}}"#).unwrap();
        drop(f);

        assert!(classify_transcript_401(&path).is_none());
    }
}
