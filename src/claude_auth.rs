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
#[cfg_attr(test, derive(Debug, Clone))]
struct KeychainEnvelope {
    #[serde(rename = "claudeAiOauth")]
    claude_ai_oauth: Option<ClaudeAiOauth>,
}

#[derive(Deserialize)]
#[cfg_attr(test, derive(Debug, Clone))]
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

fn check_token_expiry(creds: Option<&ClaudeAiOauth>, now_ms: i64) -> Result<(), AuthError> {
    let Some(creds) = creds else {
        return Ok(());
    };

    if creds.refresh_token.is_none() {
        return Ok(());
    }

    if creds.expires_at - now_ms > EXPIRY_MARGIN_MS {
        return Ok(());
    }

    Err(AuthError::Expired {
        expires_at: creds.expires_at,
    })
}

/// Check that the Claude auth token has not expired (or is not about
/// to expire within 5 minutes). Returns `Ok(())` when no keychain
/// entry exists or the session has no refresh token, treating these as
/// API-key-only paths that skip the check.
pub fn ensure_not_expired() -> Result<(), AuthError> {
    check_token_expiry(
        read_keychain().as_ref(),
        chrono::Utc::now().timestamp_millis(),
    )
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

    // -- user_message tests --

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
        assert!(msg.contains("rejected"), "should mention rejected: {msg}");
        assert!(msg.contains("401"), "should mention 401: {msg}");
        assert!(
            msg.contains("claude /login"),
            "should name recovery action: {msg}"
        );
    }

    // -- KeychainEnvelope deserialization tests --

    #[test]
    fn keychain_envelope_deserializes_with_refresh_token() {
        let json = r#"{"claudeAiOauth":{"accessToken":"at","refreshToken":"rt","expiresAt":1700000000000}}"#;
        let envelope: KeychainEnvelope = serde_json::from_str(json).unwrap();
        let creds = envelope.claude_ai_oauth.unwrap();
        assert_eq!(creds.refresh_token.as_deref(), Some("rt"));
        assert_eq!(creds.expires_at, 1_700_000_000_000);
    }

    #[test]
    fn keychain_envelope_deserializes_without_refresh_token() {
        let json = r#"{"claudeAiOauth":{"accessToken":"at","expiresAt":1700000000000}}"#;
        let envelope: KeychainEnvelope = serde_json::from_str(json).unwrap();
        let creds = envelope.claude_ai_oauth.unwrap();
        assert!(creds.refresh_token.is_none());
        assert_eq!(creds.expires_at, 1_700_000_000_000);
    }

    #[test]
    fn keychain_envelope_deserializes_without_claude_ai_oauth() {
        let json = r#"{"otherField":"value"}"#;
        let envelope: KeychainEnvelope = serde_json::from_str(json).unwrap();
        assert!(envelope.claude_ai_oauth.is_none());
    }

    // -- check_token_expiry tests --

    #[test]
    fn check_token_expiry_returns_ok_when_no_creds() {
        assert!(check_token_expiry(None, 1_700_000_000_000).is_ok());
    }

    #[test]
    fn check_token_expiry_returns_ok_when_no_refresh_token() {
        let creds = ClaudeAiOauth {
            refresh_token: None,
            expires_at: 1_700_000_000_000,
        };
        assert!(check_token_expiry(Some(&creds), 1_700_000_000_000).is_ok());
    }

    #[test]
    fn check_token_expiry_returns_ok_when_more_than_5min_remaining() {
        let now_ms = 1_700_000_000_000i64;
        let creds = ClaudeAiOauth {
            refresh_token: Some("rt".into()),
            expires_at: now_ms + EXPIRY_MARGIN_MS + 60_000,
        };
        assert!(check_token_expiry(Some(&creds), now_ms).is_ok());
    }

    #[test]
    fn check_token_expiry_returns_expired_within_margin() {
        let now_ms = 1_700_000_000_000i64;
        let creds = ClaudeAiOauth {
            refresh_token: Some("rt".into()),
            expires_at: now_ms + EXPIRY_MARGIN_MS - 1_000,
        };
        let err = check_token_expiry(Some(&creds), now_ms).unwrap_err();
        assert!(matches!(err, AuthError::Expired { .. }));
    }

    #[test]
    fn check_token_expiry_returns_expired_when_already_expired() {
        let now_ms = 1_700_000_000_000i64;
        let creds = ClaudeAiOauth {
            refresh_token: Some("rt".into()),
            expires_at: now_ms - 60_000,
        };
        let err = check_token_expiry(Some(&creds), now_ms).unwrap_err();
        match err {
            AuthError::Expired { expires_at } => {
                assert_eq!(expires_at, now_ms - 60_000);
            }
            _ => panic!("expected Expired variant"),
        }
    }

    #[test]
    fn check_token_expiry_boundary_at_exactly_5min() {
        let now_ms = 1_700_000_000_000i64;

        let creds_at_margin = ClaudeAiOauth {
            refresh_token: Some("rt".into()),
            expires_at: now_ms + EXPIRY_MARGIN_MS,
        };
        assert!(
            check_token_expiry(Some(&creds_at_margin), now_ms).is_err(),
            "exactly at margin (remaining == EXPIRY_MARGIN_MS) should be expired"
        );

        let creds_past_margin = ClaudeAiOauth {
            refresh_token: Some("rt".into()),
            expires_at: now_ms + EXPIRY_MARGIN_MS + 1,
        };
        assert!(
            check_token_expiry(Some(&creds_past_margin), now_ms).is_ok(),
            "1ms past margin should be ok"
        );
    }

    // -- classify_transcript_401 tests --

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

    #[test]
    fn classify_transcript_401_skips_malformed_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let mut f = File::create(&path).unwrap();
        writeln!(f, "not valid json at all").unwrap();
        writeln!(f, r#"{{"type":"system","subtype":"init"}}"#).unwrap();
        writeln!(f, "{{broken json").unwrap();
        writeln!(
            f,
            r#"{{"type":"result","api_error_status":401,"request_id":"req-ok"}}"#
        )
        .unwrap();
        writeln!(f, "trailing garbage").unwrap();
        drop(f);

        let err =
            classify_transcript_401(&path).expect("should detect 401 despite malformed lines");
        match err {
            AuthError::Rejected { request_id } => {
                assert_eq!(request_id.as_deref(), Some("req-ok"));
            }
            _ => panic!("expected Rejected variant"),
        }
    }
}
