//! Validate the owned Pi session before publishing its snapshot.

use anyhow::{Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs::OpenOptions;
use std::io::{Read, Seek, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

// A Pi header contains identity and path metadata, not conversation messages.
// Bound both parsing and diagnostic reads if the first record is corrupt.
const SAMPLE_LIMIT: usize = 64 * 1024;

fn value_kind(value: Option<&Value>) -> &'static str {
    match value {
        None => "missing",
        Some(Value::Null) => "null",
        Some(Value::Bool(_)) => "boolean",
        Some(Value::Number(_)) => "number",
        Some(Value::String(_)) => "string",
        Some(Value::Array(_)) => "array",
        Some(Value::Object(_)) => "object",
    }
}

fn fingerprint(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn retain_diagnostic(artifact: &Path, generation: &str, diagnostic: &Value) -> Result<PathBuf> {
    // Recovery can replan the same Task and reuse its artifact area. Keep each
    // generation's failure immutable, even if validation is repeated. Hash the
    // generation so it cannot introduce path components into the artifact name.
    let path = artifact.join(format!(
        "pi-session-validation-{}.json",
        fingerprint(generation.as_bytes())
    ));
    let mut temporary = tempfile::NamedTempFile::new_in(artifact)?;
    serde_json::to_writer_pretty(&mut temporary, diagnostic)?;
    temporary.write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    temporary.persist_noclobber(&path)?;
    Ok(path)
}

pub(super) fn save_snapshot(
    session_file: &Path,
    expected_id: &str,
    generation: &str,
    artifact: &Path,
) -> Result<()> {
    let mut diagnostic = json!({
        "schema_version": 1,
        "stage": "post-settlement-before-snapshot",
        "session_file": session_file,
        "execution_generation": generation,
        "expected_header_type": "session",
        "expected_session_id_sha256": fingerprint(expected_id.as_bytes()),
        "read_limit_bytes": SAMPLE_LIMIT,
    });
    let result = (|| -> Result<()> {
        diagnostic["failure"] = json!("open-session");
        // Refuse a replaced symlink or special file. Never search another path
        // for a session that could make the failed invocation appear complete.
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(session_file)?;
        diagnostic["failure"] = json!("session-metadata");
        let metadata = file.metadata()?;
        diagnostic["file"] = json!({
            "bytes": metadata.len(), "device": metadata.dev(), "inode": metadata.ino(),
            "modified_seconds": metadata.mtime(), "modified_nanoseconds": metadata.mtime_nsec(),
            "regular": metadata.is_file(),
        });
        ensure!(metadata.is_file(), "Pi session is not a regular file");
        diagnostic["failure"] = json!("read-session");
        let mut bytes = Vec::new();
        (&mut file)
            .take(SAMPLE_LIMIT as u64)
            .read_to_end(&mut bytes)?;
        let entire_file = metadata.len() == bytes.len() as u64;
        diagnostic["sample"] = json!({
            "bytes": bytes.len(), "sha256": fingerprint(&bytes),
            "covers_entire_file": entire_file,
        });
        let newline = bytes.iter().position(|byte| *byte == b'\n');
        let first_line = &bytes[..newline.unwrap_or(bytes.len())];
        let complete = newline.is_some() || entire_file;
        let header = complete
            .then(|| serde_json::from_slice::<Value>(first_line).ok())
            .flatten();
        // Only export fixed field names, categories, counts and fingerprints.
        // Unknown keys and string values can contain messages or credentials.
        let record_type = match header.as_ref().and_then(|value| value["type"].as_str()) {
            Some(
                kind @ ("session"
                | "message"
                | "compaction"
                | "branch_summary"
                | "model_change"
                | "thinking_level_change"
                | "custom"
                | "custom_message"
                | "label"
                | "session_info"),
            ) => kind,
            Some(_) => "other-string",
            None => "not-a-string",
        };
        let id = header.as_ref().and_then(|value| value["id"].as_str());
        let known_fields: Vec<_> = [
            "type",
            "id",
            "version",
            "timestamp",
            "cwd",
            "parentSession",
            "message",
            "parentId",
        ]
        .into_iter()
        .filter(|key| {
            header
                .as_ref()
                .is_some_and(|value| value.get(key).is_some())
        })
        .collect();
        diagnostic["first_line"] = json!({
            "bytes": first_line.len(), "complete": complete,
            "newline_terminated": newline.is_some(),
            "json_kind": if complete { header.as_ref().map(|value| value_kind(Some(value))).unwrap_or("invalid") } else { "not-parsed" },
            "record_type": record_type,
            "known_fields": known_fields,
            "field_count": header.as_ref().and_then(Value::as_object).map(|value| value.len()),
            "version": header.as_ref().and_then(|value| value["version"].as_u64()),
            "id_kind": value_kind(header.as_ref().and_then(|value| value.get("id"))),
            "id_sha256": id.map(|value| fingerprint(value.as_bytes())),
            "id_matches_expected": id == Some(expected_id),
        });
        diagnostic["failure"] = json!("empty-session");
        ensure!(!bytes.is_empty(), "Pi session is empty");
        diagnostic["failure"] = json!("header-bound");
        ensure!(complete, "Pi session header exceeds diagnostic read bound");
        diagnostic["failure"] = json!("header-json");
        let header =
            header.ok_or_else(|| anyhow::anyhow!("Pi session header is not valid JSON"))?;
        diagnostic["failure"] = json!("header-type");
        ensure!(header["type"] == "session", "Pi session has no header");
        diagnostic["failure"] = json!("header-id");
        let id = header["id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Pi session has no identity"))?;
        diagnostic["failure"] = json!("identity-mismatch");
        ensure!(
            id == expected_id,
            "Pi session identity changed before evidence copy"
        );

        diagnostic["failure"] = json!("copy-session");
        // Copy from the validated descriptor, without reopening a mutable path.
        file.rewind()?;
        let mut snapshot = tempfile::NamedTempFile::new_in(artifact)?;
        std::io::copy(&mut file, &mut snapshot)?;
        snapshot.as_file().sync_all()?;
        snapshot.persist(artifact.join("pi-session.jsonl"))?;
        Ok(())
    })();
    if let Err(error) = result {
        if let Some(io_error) = error.downcast_ref::<std::io::Error>() {
            diagnostic["io_error_kind"] = json!(format!("{:?}", io_error.kind()));
            diagnostic["io_error_code"] = json!(io_error.raw_os_error());
        }
        let path = match retain_diagnostic(artifact, generation, &diagnostic) {
            Ok(path) => path,
            Err(diagnostic_error) => {
                return Err(anyhow::anyhow!(
                    "{error:#}; could not retain Pi session validation diagnostics: {diagnostic_error:#}"
                ));
            }
        };
        return Err(error.context(format!(
            "Pi session validation failed; diagnostics: {}",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use sha2::{Digest, Sha256};
    use std::fs;

    fn diagnostic_path(artifact: &Path, generation: &str) -> std::path::PathBuf {
        artifact.join(format!(
            "pi-session-validation-{:x}.json",
            Sha256::digest(generation.as_bytes())
        ))
    }

    #[test]
    fn failed_generations_retain_both_diagnostics_in_the_same_task_area() {
        // Provider recovery reuses the Task's artifact area, but gives each
        // invocation a new execution generation and a fresh private home.
        let artifact = tempfile::tempdir().unwrap();
        let mut retained = Vec::new();
        for (generation, bytes, expected_error, failure) in [
            (
                "generation-1",
                b"{\"type\":\"message\",\"message\":\"secret-payload-one\"}\n".as_slice(),
                "Pi session has no header",
                "header-type",
            ),
            (
                "generation-2",
                b"{\"type\":\"session\",\"id\":\"secret-payload-two\"}\n",
                "Pi session identity changed before evidence copy",
                "identity-mismatch",
            ),
        ] {
            let private_home = tempfile::tempdir_in(artifact.path()).unwrap();
            let session = private_home.path().join("session.jsonl");
            fs::write(&session, bytes).unwrap();
            let error = save_snapshot(&session, "expected-session", generation, artifact.path())
                .unwrap_err();
            let path = std::path::PathBuf::from(
                error
                    .to_string()
                    .strip_prefix("Pi session validation failed; diagnostics: ")
                    .expect("the error identifies the retained diagnostic"),
            );
            let error_text = format!("{error:#}");
            assert!(error_text.contains(expected_error), "{error_text}");
            assert!(error_text.contains(path.to_str().unwrap()), "{error_text}");
            private_home.close().unwrap();
            assert!(!session.exists());
            assert!(!artifact.path().join("pi-session.jsonl").exists());
            let raw = fs::read(&path).expect("each failed generation retains its own diagnostic");
            assert!(raw.len() < 4096);
            assert!(!String::from_utf8_lossy(&raw).contains("secret-payload"));
            let diagnostic: Value = serde_json::from_slice(&raw).unwrap();
            assert_eq!(diagnostic["execution_generation"], generation);
            assert_eq!(diagnostic["session_file"], session.to_str().unwrap());
            assert_eq!(diagnostic["failure"], failure);
            assert_eq!(diagnostic["sample"]["sha256"], fingerprint(bytes));
            retained.push((path, raw));
        }
        for (path, original) in &retained {
            assert_eq!(
                &fs::read(path).unwrap(),
                original,
                "a retry replaced a failure"
            );
        }
        assert_ne!(retained[0].0, retained[1].0);
        assert_eq!(fs::read_dir(artifact.path()).unwrap().count(), 2);
    }

    #[test]
    fn repeated_generation_never_clobbers_its_first_failure() {
        let artifact = tempfile::tempdir().unwrap();
        let private_home = tempfile::tempdir_in(artifact.path()).unwrap();
        let session = private_home.path().join("session.jsonl");
        fs::write(&session, b"{\"type\":\"message\"}\n").unwrap();
        save_snapshot(
            &session,
            "expected-session",
            "generation-1",
            artifact.path(),
        )
        .unwrap_err();
        let path = diagnostic_path(artifact.path(), "generation-1");
        let first = fs::read(&path).unwrap();
        fs::write(&session, b"invalid-json\n").unwrap();
        let error = save_snapshot(
            &session,
            "expected-session",
            "generation-1",
            artifact.path(),
        )
        .unwrap_err();
        let text = format!("{error:#}");
        assert!(
            text.contains("Pi session header is not valid JSON"),
            "{text}"
        );
        assert!(
            text.contains("could not retain Pi session validation diagnostics"),
            "{text}"
        );
        private_home.close().unwrap();
        assert_eq!(fs::read(path).unwrap(), first);
        assert!(!artifact.path().join("pi-session.jsonl").exists());
        assert_eq!(fs::read_dir(artifact.path()).unwrap().count(), 1);
    }

    #[test]
    fn valid_sessions_replace_resume_snapshot_and_preserve_failures() {
        let artifact = tempfile::tempdir().unwrap();
        let private_home = tempfile::tempdir_in(artifact.path()).unwrap();
        let session = private_home.path().join("session.jsonl");
        fs::write(&session, b"{\"type\":\"message\"}\n").unwrap();
        save_snapshot(
            &session,
            "expected-session",
            "generation-1",
            artifact.path(),
        )
        .unwrap_err();
        let diagnostic = diagnostic_path(artifact.path(), "generation-1");
        let original = fs::read(&diagnostic).unwrap();
        for generation in ["generation-2", "generation-3"] {
            let bytes = format!(
                "{{\"type\":\"session\",\"id\":\"expected-session\"}}\n{{\"type\":\"message\",\"content\":\"{generation}\"}}\n"
            );
            fs::write(&session, &bytes).unwrap();
            save_snapshot(&session, "expected-session", generation, artifact.path()).unwrap();
            assert_eq!(
                fs::read_to_string(artifact.path().join("pi-session.jsonl")).unwrap(),
                bytes
            );
            assert_eq!(fs::read(&diagnostic).unwrap(), original);
            assert!(!diagnostic_path(artifact.path(), generation).exists());
        }
        private_home.close().unwrap();
        assert_eq!(fs::read_dir(artifact.path()).unwrap().count(), 2);
    }

    fn failed_session(bytes: &[u8], expected_error: &str) -> Value {
        let artifact = tempfile::tempdir().unwrap();
        let private_home = tempfile::tempdir_in(artifact.path()).unwrap();
        let session = private_home.path().join("session.jsonl");
        fs::write(&session, bytes).unwrap();
        let error = save_snapshot(
            &session,
            "expected-session",
            "generation-1",
            artifact.path(),
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains(expected_error), "{error:#}");
        private_home.close().unwrap();
        assert!(!session.exists(), "private session must be cleaned up");
        assert!(!artifact.path().join("pi-session.jsonl").exists());
        let diagnostic = fs::read(diagnostic_path(artifact.path(), "generation-1"))
            .expect("retain structural diagnostics after private-home cleanup");
        assert!(diagnostic.len() < 4096);
        assert!(!String::from_utf8_lossy(&diagnostic).contains("secret-payload"));
        let diagnostic: Value = serde_json::from_slice(&diagnostic).unwrap();
        assert_eq!(diagnostic["stage"], "post-settlement-before-snapshot");
        assert_eq!(diagnostic["session_file"], session.to_str().unwrap());
        assert_eq!(diagnostic["execution_generation"], "generation-1");
        assert_eq!(
            diagnostic["expected_session_id_sha256"],
            format!("{:x}", Sha256::digest(b"expected-session"))
        );
        if bytes.len() <= 65536 {
            assert_eq!(
                diagnostic["sample"]["sha256"],
                format!("{:x}", Sha256::digest(bytes))
            );
            assert_eq!(diagnostic["sample"]["covers_entire_file"], true);
        }
        diagnostic
    }

    #[test]
    fn non_session_header_retains_safe_diagnostics_after_cleanup() {
        let bytes = b"{\"type\":\"message\",\"id\":\"secret-payload\",\"message\":{\"content\":\"secret-payload\"}}\n";
        let diagnostic = failed_session(bytes, "Pi session has no header");
        assert_eq!(diagnostic["failure"], "header-type");
        assert_eq!(diagnostic["file"]["bytes"], bytes.len());
        assert_eq!(diagnostic["first_line"]["json_kind"], "object");
        assert_eq!(diagnostic["first_line"]["record_type"], "message");
        assert_eq!(diagnostic["first_line"]["id_matches_expected"], false);
        assert_eq!(diagnostic["first_line"]["newline_terminated"], true);
    }

    #[test]
    fn invalid_session_structures_fail_closed_without_exporting_values() {
        for (bytes, error, failure) in [
            (b"".as_slice(), "Pi session is empty", "empty-session"),
            (
                b"secret-payload\n",
                "Pi session header is not valid JSON",
                "header-json",
            ),
            (
                b"\xff\n",
                "Pi session header is not valid JSON",
                "header-json",
            ),
            (b"[]\n", "Pi session has no header", "header-type"),
            (b"null\n", "Pi session has no header", "header-type"),
            (
                b"\"secret-payload\"\n",
                "Pi session has no header",
                "header-type",
            ),
            (
                b"{\"type\":\"session\"}\n",
                "Pi session has no identity",
                "header-id",
            ),
            (
                b"{\"type\":\"session\",\"id\":\"\"}\n",
                "Pi session has no identity",
                "header-id",
            ),
            (
                b"{\"type\":\"session\",\"id\":\"secret-payload\"}\n",
                "Pi session identity changed",
                "identity-mismatch",
            ),
            (
                b"{\"type\":\"secret-payload\",\"secret-payload\":\"secret-payload\"}\n",
                "Pi session has no header",
                "header-type",
            ),
        ] {
            assert_eq!(failed_session(bytes, error)["failure"], failure);
        }
    }

    #[test]
    fn oversized_session_header_retains_only_a_bounded_fingerprint() {
        let bytes = serde_json::to_vec(
            &json!({"type":"message", "message": "secret-payload".repeat(10000)}),
        )
        .unwrap();
        let diagnostic = failed_session(&bytes, "Pi session header exceeds diagnostic read bound");
        assert_eq!(diagnostic["sample"]["bytes"], 65536);
        assert_eq!(diagnostic["sample"]["covers_entire_file"], false);
        assert_eq!(
            diagnostic["sample"]["sha256"],
            format!("{:x}", Sha256::digest(&bytes[..65536]))
        );
        assert_eq!(diagnostic["first_line"]["complete"], false);
    }

    #[test]
    fn valid_session_preserves_snapshot_bytes() {
        let artifact = tempfile::tempdir().unwrap();
        let private_home = tempfile::tempdir_in(artifact.path()).unwrap();
        let session = private_home.path().join("session.jsonl");
        // A large body must not become a header-size restriction.
        let bytes = format!(
            "{{\"type\":\"session\",\"id\":\"expected-session\"}}\n{{\"type\":\"message\",\"content\":\"{}\"}}\n",
            "body".repeat(30000)
        );
        fs::write(&session, &bytes).unwrap();
        save_snapshot(
            &session,
            "expected-session",
            "generation-1",
            artifact.path(),
        )
        .unwrap();
        private_home.close().unwrap();
        assert_eq!(
            fs::read_to_string(artifact.path().join("pi-session.jsonl")).unwrap(),
            bytes
        );
        assert!(!diagnostic_path(artifact.path(), "generation-1").exists());
    }

    #[test]
    fn session_validation_never_follows_a_replaced_session_symlink() {
        let artifact = tempfile::tempdir().unwrap();
        let private_home = tempfile::tempdir_in(artifact.path()).unwrap();
        let other = private_home.path().join("other-session");
        fs::write(
            &other,
            b"{\"type\":\"session\",\"id\":\"expected-session\"}\n",
        )
        .unwrap();
        let session = private_home.path().join("session.jsonl");
        std::os::unix::fs::symlink(&other, &session).unwrap();
        assert!(
            save_snapshot(
                &session,
                "expected-session",
                "generation-1",
                artifact.path()
            )
            .is_err()
        );
        assert!(!artifact.path().join("pi-session.jsonl").exists());
        let diagnostic: Value = serde_json::from_slice(
            &fs::read(diagnostic_path(artifact.path(), "generation-1")).unwrap(),
        )
        .unwrap();
        assert_eq!(diagnostic["failure"], "open-session");
        assert!(diagnostic.get("sample").is_none());
    }

    #[test]
    fn missing_session_retains_validation_stage_without_a_sample() {
        let artifact = tempfile::tempdir().unwrap();
        let session = artifact.path().join("missing.jsonl");
        assert!(
            save_snapshot(
                &session,
                "expected-session",
                "generation-1",
                artifact.path()
            )
            .is_err()
        );
        let diagnostic: Value = serde_json::from_slice(
            &fs::read(diagnostic_path(artifact.path(), "generation-1")).unwrap(),
        )
        .unwrap();
        assert_eq!(diagnostic["failure"], "open-session");
        assert!(diagnostic.get("sample").is_none());
    }

    #[test]
    fn diagnostic_write_failure_preserves_the_validation_error() {
        let artifact = tempfile::tempdir().unwrap();
        let session = artifact.path().join("session.jsonl");
        fs::write(&session, b"{\"type\":\"message\"}\n").unwrap();
        fs::create_dir(diagnostic_path(artifact.path(), "generation-1")).unwrap();
        let error = save_snapshot(
            &session,
            "expected-session",
            "generation-1",
            artifact.path(),
        )
        .unwrap_err();
        let text = format!("{error:#}");
        assert!(text.contains("Pi session has no header"));
        assert!(text.contains("could not retain Pi session validation diagnostics"));
        assert!(!artifact.path().join("pi-session.jsonl").exists());
    }
}
