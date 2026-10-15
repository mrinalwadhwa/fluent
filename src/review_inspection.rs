use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::git;

pub(crate) const REVIEW_INSPECTION_FILE: &str = "review-inspection.json";
const REVIEW_INSPECTION_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) struct ReviewInspection {
    pub(crate) schema_version: u32,
    pub(crate) candidate_commit: String,
    pub(crate) role: String,
    pub(crate) inspections: Vec<Inspection>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) struct Inspection {
    pub(crate) path: String,
    pub(crate) summary: String,
}

pub(crate) fn validate(
    inspection_path: &Path,
    candidate_workspace: &Path,
    transcript_path: &Path,
    expected_candidate_commit: &str,
    expected_role: &str,
) -> Result<ReviewInspection> {
    let source = fs::read_to_string(inspection_path).with_context(|| {
        format!(
            "passing Review Task did not write readable inspection evidence at {}",
            inspection_path.display()
        )
    })?;
    let inspection: ReviewInspection = serde_json::from_str(&source).with_context(|| {
        format!(
            "passing Review Task wrote invalid inspection evidence at {}",
            inspection_path.display()
        )
    })?;

    if inspection.schema_version != REVIEW_INSPECTION_SCHEMA_VERSION {
        bail!(
            "review inspection schema-version must be {}, got {}",
            REVIEW_INSPECTION_SCHEMA_VERSION,
            inspection.schema_version
        );
    }
    if inspection.candidate_commit != expected_candidate_commit {
        bail!(
            "review inspection candidate-commit {:?} does not match reviewed candidate {:?}",
            inspection.candidate_commit,
            expected_candidate_commit
        );
    }
    if inspection.role != expected_role {
        bail!(
            "review inspection role {:?} does not match Review Task role {:?}",
            inspection.role,
            expected_role
        );
    }
    if inspection.inspections.is_empty() {
        bail!("passing review inspection must name at least one inspected candidate file");
    }

    let mut unique_paths = BTreeSet::new();
    let mut inspected_paths = Vec::new();
    for item in &inspection.inspections {
        let path = canonical_relative_path(&item.path)?;
        if item.summary.trim().is_empty() {
            bail!(
                "review inspection for {:?} must explain what the reviewer checked",
                item.path
            );
        }
        if !unique_paths.insert(item.path.as_str()) {
            bail!("review inspection repeats candidate path {:?}", item.path);
        }
        require_file_at_commit(candidate_workspace, expected_candidate_commit, &path)?;
        inspected_paths.push((
            item.path.clone(),
            candidate_workspace.join(path).display().to_string(),
        ));
    }
    if !transcript_records_successful_inspection(
        transcript_path,
        &candidate_workspace.display().to_string(),
        expected_candidate_commit,
        &inspected_paths,
    )? {
        bail!(
            "passing review has no successful provider tool result that identifies the candidate and a declared inspected file"
        );
    }

    Ok(inspection)
}

fn transcript_records_successful_inspection(
    transcript_path: &Path,
    candidate_workspace: &str,
    candidate_commit: &str,
    inspected_paths: &[(String, String)],
) -> Result<bool> {
    let source = fs::read_to_string(transcript_path).with_context(|| {
        format!(
            "passing Review Task did not leave a readable transcript at {}",
            transcript_path.display()
        )
    })?;
    let mut claude_tool_inputs = BTreeMap::<String, serde_json::Value>::new();
    let mut pi_tool_inputs = BTreeMap::<(bool, String), serde_json::Value>::new();

    for line in source.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            // Provider wrappers may write human-readable console lines beside the
            // canonical stream. They are not inspection evidence, but they must not
            // hide a later valid tool event either.
            continue;
        };

        let legacy_codex = value.get("type").and_then(|value| value.as_str())
            == Some("item.completed")
            && value["item"].get("type").and_then(|value| value.as_str())
                == Some("command_execution")
            && value["item"].get("status").and_then(|value| value.as_str()) == Some("completed")
            && value["item"]
                .get("exit_code")
                .and_then(|value| value.as_i64())
                == Some(0);
        let native_codex = value.get("format").and_then(|value| value.as_str())
            == Some("fluent-native-agent-event")
            && value.get("provider").and_then(|value| value.as_str()) == Some("codex")
            && value.get("schema_version").and_then(|value| value.as_u64()) == Some(1)
            && value["event"]
                .get("method")
                .and_then(|value| value.as_str())
                == Some("item/completed")
            && value["event"]["params"]["item"]
                .get("type")
                .and_then(|value| value.as_str())
                == Some("commandExecution")
            && value["event"]["params"]["item"]
                .get("exitCode")
                .and_then(|value| value.as_i64())
                == Some(0);
        if legacy_codex || native_codex {
            let item = if native_codex {
                &value["event"]["params"]["item"]
            } else {
                &value["item"]
            };
            let evidence = format!(
                "{}\n{}",
                item.get("command")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default(),
                item.get(if native_codex {
                    "aggregatedOutput"
                } else {
                    "aggregated_output"
                })
                .and_then(|value| value.as_str())
                .unwrap_or_default()
            );
            if text_names_any_candidate_path(
                &evidence,
                candidate_workspace,
                candidate_commit,
                inspected_paths,
            ) {
                return Ok(true);
            }
        }

        if value.get("type").and_then(|value| value.as_str()) == Some("assistant") {
            for block in value["message"]["content"].as_array().into_iter().flatten() {
                if block.get("type").and_then(|value| value.as_str()) == Some("tool_use") {
                    if let Some(id) = block.get("id").and_then(|value| value.as_str()) {
                        claude_tool_inputs.insert(id.to_string(), block["input"].clone());
                    }
                }
            }
        }

        if value.get("type").and_then(|value| value.as_str()) == Some("user") {
            for block in value["message"]["content"].as_array().into_iter().flatten() {
                if block.get("type").and_then(|value| value.as_str()) != Some("tool_result")
                    || block.get("is_error").and_then(|value| value.as_bool()) == Some(true)
                {
                    continue;
                }
                let Some(tool_use_id) = block.get("tool_use_id").and_then(|value| value.as_str())
                else {
                    continue;
                };
                let Some(input) = claude_tool_inputs.get(tool_use_id) else {
                    continue;
                };
                if value_names_any_candidate_path(
                    input,
                    candidate_workspace,
                    candidate_commit,
                    inspected_paths,
                ) {
                    return Ok(true);
                }
            }
        }

        let native_pi = value.get("format").and_then(|value| value.as_str())
            == Some("fluent-native-agent-event")
            && value.get("provider").and_then(|value| value.as_str()) == Some("pi")
            && value.get("schema_version").and_then(|value| value.as_u64()) == Some(1);
        let pi_event = if native_pi { &value["event"] } else { &value };
        if pi_event.get("type").and_then(|value| value.as_str()) == Some("tool_execution_start") {
            if let Some(id) = pi_event.get("toolCallId").and_then(|value| value.as_str()) {
                pi_tool_inputs.insert((native_pi, id.to_string()), pi_event["args"].clone());
            }
        }

        if pi_event.get("type").and_then(|value| value.as_str()) == Some("tool_execution_end")
            && pi_event.get("isError").and_then(|value| value.as_bool()) == Some(false)
        {
            let Some(tool_call_id) = pi_event.get("toolCallId").and_then(|value| value.as_str())
            else {
                continue;
            };
            let Some(input) = pi_tool_inputs.get(&(native_pi, tool_call_id.to_string())) else {
                continue;
            };
            if value_names_any_candidate_path(
                input,
                candidate_workspace,
                candidate_commit,
                inspected_paths,
            ) {
                return Ok(true);
            }
        }
    }

    Ok(false)
}

fn text_names_any_candidate_path(
    source: &str,
    candidate_workspace: &str,
    candidate_commit: &str,
    inspected_paths: &[(String, String)],
) -> bool {
    inspected_paths.iter().any(|(relative, absolute)| {
        text_names_path(source, absolute)
            || (text_names_path(source, candidate_workspace) && text_names_path(source, relative))
            || (text_names_path(source, candidate_commit) && text_names_path(source, relative))
    })
}

fn value_names_any_candidate_path(
    value: &serde_json::Value,
    candidate_workspace: &str,
    candidate_commit: &str,
    inspected_paths: &[(String, String)],
) -> bool {
    inspected_paths.iter().any(|(relative, absolute)| {
        value_names_path(value, absolute)
            || (value_names_path(value, candidate_workspace) && value_names_path(value, relative))
            || (value_names_path(value, candidate_commit) && value_names_path(value, relative))
    })
}

fn value_names_path(value: &serde_json::Value, path: &str) -> bool {
    match value {
        serde_json::Value::String(source) => text_names_path(source, path),
        serde_json::Value::Array(values) => {
            values.iter().any(|value| value_names_path(value, path))
        }
        serde_json::Value::Object(values) => {
            values.values().any(|value| value_names_path(value, path))
        }
        _ => false,
    }
}

fn text_names_path(source: &str, path: &str) -> bool {
    source.match_indices(path).any(|(start, _)| {
        let before = source[..start].chars().next_back();
        let after = source[start + path.len()..].chars().next();
        !before.is_some_and(is_path_continuation) && !after.is_some_and(is_path_continuation)
    })
}

fn is_path_continuation(value: char) -> bool {
    value.is_alphanumeric() || matches!(value, '_' | '-' | '.' | '/' | '\\')
}

fn canonical_relative_path(source: &str) -> Result<PathBuf> {
    let path = Path::new(source);
    if source.is_empty() || path.is_absolute() {
        bail!("review inspection path must be a non-empty relative path: {source:?}");
    }

    let mut rebuilt = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => rebuilt.push(value),
            _ => bail!("review inspection path is not canonically spelled: {source:?}"),
        }
    }
    if rebuilt.as_os_str() != OsStr::new(source) {
        bail!("review inspection path is not canonically spelled: {source:?}");
    }
    Ok(rebuilt)
}

fn require_file_at_commit(workspace: &Path, commit: &str, path: &Path) -> Result<()> {
    let path = path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("review inspection path is not valid UTF-8"))?;
    let literal_pathspec = format!(":(literal){path}");
    let output = git::run_raw(
        workspace,
        &[
            "ls-tree",
            "-z",
            "--full-tree",
            commit,
            "--",
            &literal_pathspec,
        ],
    )?;
    if !output.status.success() {
        bail!(
            "could not verify review inspection path {:?} at candidate commit {:?}: {}",
            path,
            commit,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let entry = output
        .stdout
        .strip_suffix(&[0])
        .unwrap_or(output.stdout.as_slice());
    let header = entry
        .split(|byte| *byte == b'\t')
        .next()
        .unwrap_or_default();
    let object_type = header
        .split(|byte| *byte == b' ')
        .nth(1)
        .unwrap_or_default();
    if object_type != b"blob" {
        bail!(
            "review inspection path {:?} is not a tracked file at candidate commit {:?}",
            path,
            commit
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn committed_workspace() -> (TempDir, String) {
        let workspace = TempDir::new().unwrap();
        git::run(workspace.path(), &["init", "-q"], "initialize fixture").unwrap();
        fs::create_dir_all(workspace.path().join("src")).unwrap();
        fs::write(
            workspace.path().join("src/lib.rs"),
            "pub fn answer() -> u8 { 42 }\n",
        )
        .unwrap();
        git::run(workspace.path(), &["add", "src/lib.rs"], "stage fixture").unwrap();
        git::run(
            workspace.path(),
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-q",
                "-m",
                "fixture",
            ],
            "commit fixture",
        )
        .unwrap();
        let commit =
            git::run_stdout(workspace.path(), &["rev-parse", "HEAD"], "read HEAD").unwrap();
        (workspace, commit)
    }

    fn write_inspection(
        root: &Path,
        commit: &str,
        role: &str,
        path: &str,
        summary: &str,
    ) -> PathBuf {
        let artifact = root.join(REVIEW_INSPECTION_FILE);
        let value = serde_json::json!({
            "schema-version": 1,
            "candidate-commit": commit,
            "role": role,
            "inspections": [{"path": path, "summary": summary}],
        });
        fs::write(&artifact, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
        artifact
    }

    #[test]
    fn accepts_candidate_bound_file_inspection() {
        let (workspace, commit) = committed_workspace();
        let artifact = write_inspection(
            workspace.path(),
            &commit,
            "architecture",
            "src/lib.rs",
            "Checked the public module boundary.",
        );

        let transcript = workspace.path().join("transcript.jsonl");
        fs::write(
            &transcript,
            format!(
                "{{\"type\":\"item.completed\",\"item\":{{\"type\":\"command_execution\",\"status\":\"completed\",\"exit_code\":0,\"command\":\"sed -n 1,40p {}\"}}}}\n",
                workspace.path().join("src/lib.rs").display()
            ),
        )
        .unwrap();
        let validated = validate(
            &artifact,
            workspace.path(),
            &transcript,
            &commit,
            "architecture",
        )
        .unwrap();

        assert_eq!(validated.inspections[0].path, "src/lib.rs");
    }

    #[test]
    fn accepts_successful_codex_git_object_inspection() {
        let (workspace, commit) = committed_workspace();
        let artifact = write_inspection(
            workspace.path(),
            &commit,
            "architecture",
            "src/lib.rs",
            "Checked the public module boundary at the candidate commit.",
        );
        let transcript = workspace.path().join("transcript.jsonl");
        fs::write(
            &transcript,
            format!(
                "{{\"type\":\"item.completed\",\"item\":{{\"type\":\"command_execution\",\"status\":\"completed\",\"exit_code\":0,\"command\":\"git -C '{}' show '{}:src/lib.rs'\"}}}}\n",
                workspace.path().display(),
                commit,
            ),
        )
        .unwrap();

        validate(
            &artifact,
            workspace.path(),
            &transcript,
            &commit,
            "architecture",
        )
        .unwrap();
    }

    #[test]
    fn accepts_successful_codex_commit_object_inspection_without_workspace() {
        let (workspace, commit) = committed_workspace();
        let artifact = write_inspection(
            workspace.path(),
            &commit,
            "skills",
            "src/lib.rs",
            "Checked the exact candidate object and found no skill surface.",
        );
        let transcript = workspace.path().join("transcript.jsonl");
        fs::write(
            &transcript,
            format!(
                "{{\"type\":\"item.completed\",\"item\":{{\"type\":\"command_execution\",\"status\":\"completed\",\"exit_code\":0,\"command\":\"git show '{}:src/lib.rs'\"}}}}\n",
                commit,
            ),
        )
        .unwrap();

        validate(&artifact, workspace.path(), &transcript, &commit, "skills").unwrap();
    }

    #[test]
    fn accepts_successful_native_codex_inspection_and_rejects_failed_command() {
        let (workspace, commit) = committed_workspace();
        let artifact = write_inspection(
            workspace.path(),
            &commit,
            "tests",
            "src/lib.rs",
            "Checked the candidate source file.",
        );
        let transcript = workspace.path().join("transcript.jsonl");
        let command = format!("git show '{commit}:src/lib.rs'");
        let event = |exit_code| {
            serde_json::json!({
                "format": "fluent-native-agent-event",
                "schema_version": 1,
                "provider": "codex",
                "event": {
                    "method": "item/completed",
                    "params": {
                        "item": {
                            "type": "commandExecution",
                            "command": command,
                            "exitCode": exit_code,
                            "aggregatedOutput": "pub fn answer() -> u8 { 42 }\n",
                        }
                    }
                }
            })
            .to_string()
        };

        fs::write(&transcript, format!("{}\n", event(0))).unwrap();
        validate(&artifact, workspace.path(), &transcript, &commit, "tests").unwrap();

        fs::write(&transcript, format!("{}\n", event(1))).unwrap();
        let error =
            validate(&artifact, workspace.path(), &transcript, &commit, "tests").unwrap_err();
        assert!(
            error
                .to_string()
                .contains("no successful provider tool result")
        );
    }

    #[test]
    fn rejects_candidate_mismatch() {
        let (workspace, commit) = committed_workspace();
        let artifact = write_inspection(
            workspace.path(),
            "0000000000000000000000000000000000000000",
            "tests",
            "src/lib.rs",
            "Checked coverage.",
        );

        let error = validate(
            &artifact,
            workspace.path(),
            &workspace.path().join("transcript.jsonl"),
            &commit,
            "architecture",
        )
        .unwrap_err();

        assert!(error.to_string().contains("candidate-commit"));
    }

    #[test]
    fn rejects_role_mismatch() {
        let (workspace, commit) = committed_workspace();
        let artifact = write_inspection(
            workspace.path(),
            &commit,
            "tests",
            "src/lib.rs",
            "Checked coverage.",
        );

        let error = validate(
            &artifact,
            workspace.path(),
            &workspace.path().join("transcript.jsonl"),
            &commit,
            "architecture",
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("does not match Review Task role")
        );
    }

    #[test]
    fn rejects_wrong_schema_version() {
        let (workspace, commit) = committed_workspace();
        let artifact = workspace.path().join(REVIEW_INSPECTION_FILE);
        let value = serde_json::json!({
            "schema-version": 2,
            "candidate-commit": commit,
            "role": "architecture",
            "inspections": [{
                "path": "src/lib.rs",
                "summary": "Checked the public module boundary."
            }],
        });
        fs::write(&artifact, serde_json::to_vec_pretty(&value).unwrap()).unwrap();

        let error = validate(
            &artifact,
            workspace.path(),
            &workspace.path().join("transcript.jsonl"),
            &commit,
            "architecture",
        )
        .unwrap_err();

        assert!(error.to_string().contains("schema-version must be 1"));
    }

    #[test]
    fn rejects_empty_inspection_list() {
        let (workspace, commit) = committed_workspace();
        let artifact = workspace.path().join(REVIEW_INSPECTION_FILE);
        let value = serde_json::json!({
            "schema-version": 1,
            "candidate-commit": commit,
            "role": "architecture",
            "inspections": [],
        });
        fs::write(&artifact, serde_json::to_vec_pretty(&value).unwrap()).unwrap();

        let error = validate(
            &artifact,
            workspace.path(),
            &workspace.path().join("transcript.jsonl"),
            &commit,
            "architecture",
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("at least one inspected candidate file")
        );
    }

    #[test]
    fn rejects_missing_empty_or_noncanonical_inspections() {
        let (workspace, commit) = committed_workspace();
        for (path, summary, expected) in [
            ("missing.rs", "Checked it.", "not a tracked file"),
            ("src", "Checked it.", "not a tracked file"),
            ("src/./lib.rs", "Checked it.", "not canonically spelled"),
            ("/src/lib.rs", "Checked it.", "non-empty relative path"),
            (
                "src/../src/lib.rs",
                "Checked it.",
                "not canonically spelled",
            ),
            ("src//lib.rs", "Checked it.", "not canonically spelled"),
            ("src/lib.rs/", "Checked it.", "not canonically spelled"),
            ("src/lib.rs", "   ", "must explain"),
        ] {
            let artifact =
                write_inspection(workspace.path(), &commit, "architecture", path, summary);
            let error = validate(
                &artifact,
                workspace.path(),
                &workspace.path().join("transcript.jsonl"),
                &commit,
                "architecture",
            )
            .unwrap_err();
            assert!(
                error.to_string().contains(expected),
                "expected {expected:?}, got {error:#}"
            );
        }
    }

    #[test]
    fn rejects_duplicate_inspection_paths() {
        let (workspace, commit) = committed_workspace();
        let artifact = workspace.path().join(REVIEW_INSPECTION_FILE);
        let value = serde_json::json!({
            "schema-version": 1,
            "candidate-commit": commit,
            "role": "architecture",
            "inspections": [
                {"path": "src/lib.rs", "summary": "Checked the module boundary."},
                {"path": "src/lib.rs", "summary": "Checked the same boundary again."},
            ],
        });
        fs::write(&artifact, serde_json::to_vec_pretty(&value).unwrap()).unwrap();

        let error = validate(
            &artifact,
            workspace.path(),
            &workspace.path().join("transcript.jsonl"),
            &commit,
            "architecture",
        )
        .unwrap_err();

        assert!(error.to_string().contains("repeats candidate path"));
    }

    #[test]
    fn rejects_transcript_without_tool_inspection() {
        let (workspace, commit) = committed_workspace();
        let artifact = write_inspection(
            workspace.path(),
            &commit,
            "architecture",
            "src/lib.rs",
            "Checked the public module boundary.",
        );
        let transcript = workspace.path().join("transcript.jsonl");
        fs::write(
            &transcript,
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\"}}\n",
        )
        .unwrap();
        let error = validate(
            &artifact,
            workspace.path(),
            &transcript,
            &commit,
            "architecture",
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("no successful provider tool result")
        );
    }

    #[test]
    fn rejects_partial_wrong_and_cross_event_candidate_evidence() {
        let (workspace, commit) = committed_workspace();
        let artifact = write_inspection(
            workspace.path(),
            &commit,
            "architecture",
            "src/lib.rs",
            "Checked the public module boundary.",
        );
        let workspace_path = workspace.path().display();

        for (label, source) in [
            (
                "workspace only",
                format!(
                    "{{\"type\":\"item.completed\",\"item\":{{\"type\":\"command_execution\",\"status\":\"completed\",\"exit_code\":0,\"command\":\"git -C '{workspace_path}' status\"}}}}\n"
                ),
            ),
            (
                "relative path only",
                "{\"type\":\"item.completed\",\"item\":{\"type\":\"command_execution\",\"status\":\"completed\",\"exit_code\":0,\"command\":\"sed -n 1,40p src/lib.rs\"}}\n".to_string(),
            ),
            (
                "commit only",
                format!(
                    "{{\"type\":\"item.completed\",\"item\":{{\"type\":\"command_execution\",\"status\":\"completed\",\"exit_code\":0,\"command\":\"git show --stat {commit}\"}}}}\n"
                ),
            ),
            (
                "wrong commit",
                "{\"type\":\"item.completed\",\"item\":{\"type\":\"command_execution\",\"status\":\"completed\",\"exit_code\":0,\"command\":\"git show 0000000000000000000000000000000000000000:src/lib.rs\"}}\n".to_string(),
            ),
            (
                "wrong workspace",
                "{\"type\":\"item.completed\",\"item\":{\"type\":\"command_execution\",\"status\":\"completed\",\"exit_code\":0,\"command\":\"git -C /definitely/not/the/candidate show HEAD:src/lib.rs\"}}\n".to_string(),
            ),
            (
                "declared path prefix only",
                format!(
                    "{{\"type\":\"item.completed\",\"item\":{{\"type\":\"command_execution\",\"status\":\"completed\",\"exit_code\":0,\"command\":\"git -C '{workspace_path}' show HEAD:src/lib.rs.bak\"}}}}\n"
                ),
            ),
            (
                "split across events",
                format!(
                    "{{\"type\":\"item.completed\",\"item\":{{\"type\":\"command_execution\",\"status\":\"completed\",\"exit_code\":0,\"command\":\"git -C '{workspace_path}' status\"}}}}\n{{\"type\":\"item.completed\",\"item\":{{\"type\":\"command_execution\",\"status\":\"completed\",\"exit_code\":0,\"command\":\"sed -n 1,40p src/lib.rs\"}}}}\n"
                ),
            ),
        ] {
            let transcript = workspace.path().join(format!("{label}.jsonl"));
            fs::write(&transcript, source).unwrap();
            let error = validate(
                &artifact,
                workspace.path(),
                &transcript,
                &commit,
                "architecture",
            )
            .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("identifies the candidate"),
                "{label}: {error:#}"
            );
        }
    }

    #[test]
    fn rejects_failed_only_codex_inspection() {
        let (workspace, commit) = committed_workspace();
        let artifact = write_inspection(
            workspace.path(),
            &commit,
            "architecture",
            "src/lib.rs",
            "Checked the public module boundary.",
        );
        let transcript = workspace.path().join("transcript.jsonl");
        fs::write(
            &transcript,
            format!(
                "{{\"type\":\"item.completed\",\"item\":{{\"type\":\"command_execution\",\"status\":\"failed\",\"exit_code\":1,\"command\":\"sed -n 1,40p {}\"}}}}\n",
                workspace.path().join("src/lib.rs").display()
            ),
        )
        .unwrap();
        let error = validate(
            &artifact,
            workspace.path(),
            &transcript,
            &commit,
            "architecture",
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("no successful provider tool result")
        );
    }

    #[test]
    fn accepts_recovered_codex_inspection() {
        let (workspace, commit) = committed_workspace();
        let artifact = write_inspection(
            workspace.path(),
            &commit,
            "skills",
            "src/lib.rs",
            "Confirmed that the change introduces no agent-skill surface.",
        );
        let inspected = workspace.path().join("src/lib.rs").display().to_string();
        let transcript = workspace.path().join("transcript.jsonl");
        fs::write(
            &transcript,
            format!(
                "{{\"type\":\"item.completed\",\"item\":{{\"type\":\"command_execution\",\"status\":\"failed\",\"exit_code\":1,\"command\":\"sed -n 1,40p {inspected}\"}}}}\n{{\"type\":\"item.completed\",\"item\":{{\"type\":\"command_execution\",\"status\":\"completed\",\"exit_code\":0,\"command\":\"sed -n 1,40p {inspected}\"}}}}\n"
            ),
        )
        .unwrap();
        validate(&artifact, workspace.path(), &transcript, &commit, "skills").unwrap();
    }

    #[test]
    fn accepts_successful_claude_inspection() {
        let (workspace, commit) = committed_workspace();
        let artifact = write_inspection(
            workspace.path(),
            &commit,
            "skills",
            "src/lib.rs",
            "Confirmed that the change introduces no agent-skill surface.",
        );
        let inspected = workspace.path().join("src/lib.rs").display().to_string();
        let transcript = workspace.path().join("transcript.jsonl");
        fs::write(
            &transcript,
            format!(
                "{{\"type\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"tool_use\",\"id\":\"tool-1\",\"name\":\"Read\",\"input\":{{\"file_path\":\"{inspected}\"}}}}]}}}}\n{{\"type\":\"user\",\"message\":{{\"content\":[{{\"type\":\"tool_result\",\"tool_use_id\":\"tool-1\",\"content\":\"source\"}}]}}}}\n"
            ),
        )
        .unwrap();
        validate(&artifact, workspace.path(), &transcript, &commit, "skills").unwrap();
    }

    #[test]
    fn accepts_successful_claude_split_workspace_and_path_inspection() {
        let (workspace, commit) = committed_workspace();
        let artifact = write_inspection(
            workspace.path(),
            &commit,
            "skills",
            "src/lib.rs",
            "Confirmed that the change introduces no agent-skill surface.",
        );
        let transcript = workspace.path().join("transcript.jsonl");
        fs::write(
            &transcript,
            format!(
                "{{\"type\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"tool_use\",\"id\":\"tool-1\",\"name\":\"Read\",\"input\":{{\"cwd\":\"{}\",\"file_path\":\"src/lib.rs\"}}}}]}}}}\n{{\"type\":\"user\",\"message\":{{\"content\":[{{\"type\":\"tool_result\",\"tool_use_id\":\"tool-1\",\"content\":\"source\"}}]}}}}\n",
                workspace.path().display()
            ),
        )
        .unwrap();
        validate(&artifact, workspace.path(), &transcript, &commit, "skills").unwrap();
    }

    #[test]
    fn accepts_successful_claude_split_commit_and_path_inspection() {
        let (workspace, commit) = committed_workspace();
        let artifact = write_inspection(
            workspace.path(),
            &commit,
            "skills",
            "src/lib.rs",
            "Checked the exact candidate object and found no skill surface.",
        );
        let transcript = workspace.path().join("transcript.jsonl");
        fs::write(
            &transcript,
            format!(
                "{{\"type\":\"assistant\",\"message\":{{\"content\":[{{\"type\":\"tool_use\",\"id\":\"tool-1\",\"name\":\"Read\",\"input\":{{\"commit\":\"{commit}\",\"file_path\":\"src/lib.rs\"}}}}]}}}}\n{{\"type\":\"user\",\"message\":{{\"content\":[{{\"type\":\"tool_result\",\"tool_use_id\":\"tool-1\",\"content\":\"source\"}}]}}}}\n"
            ),
        )
        .unwrap();
        validate(&artifact, workspace.path(), &transcript, &commit, "skills").unwrap();
    }

    #[test]
    fn rejects_failed_only_pi_inspection() {
        let (workspace, commit) = committed_workspace();
        let artifact = write_inspection(
            workspace.path(),
            &commit,
            "tests",
            "src/lib.rs",
            "Checked the candidate's test surface.",
        );
        let inspected = workspace.path().join("src/lib.rs").display().to_string();
        let transcript = workspace.path().join("transcript.jsonl");
        let start = format!(
            "{{\"type\":\"tool_execution_start\",\"toolCallId\":\"tool-1\",\"toolName\":\"read\",\"args\":{{\"path\":\"{inspected}\"}}}}\n"
        );
        fs::write(
            &transcript,
            format!(
                "{start}{{\"type\":\"tool_execution_end\",\"toolCallId\":\"tool-1\",\"toolName\":\"read\",\"result\":{{}},\"isError\":true}}\n"
            ),
        )
        .unwrap();
        let error =
            validate(&artifact, workspace.path(), &transcript, &commit, "tests").unwrap_err();
        assert!(
            error
                .to_string()
                .contains("no successful provider tool result")
        );
    }

    #[test]
    fn accepts_successful_pi_inspection() {
        let (workspace, commit) = committed_workspace();
        let artifact = write_inspection(
            workspace.path(),
            &commit,
            "tests",
            "src/lib.rs",
            "Checked the candidate's test surface.",
        );
        let inspected = workspace.path().join("src/lib.rs").display().to_string();
        let transcript = workspace.path().join("transcript.jsonl");
        let start = format!(
            "{{\"type\":\"tool_execution_start\",\"toolCallId\":\"tool-1\",\"toolName\":\"read\",\"args\":{{\"path\":\"{inspected}\"}}}}\n"
        );
        fs::write(
            &transcript,
            format!(
                "{start}{{\"type\":\"tool_execution_end\",\"toolCallId\":\"tool-1\",\"toolName\":\"read\",\"result\":{{}},\"isError\":false}}\n"
            ),
        )
        .unwrap();
        validate(&artifact, workspace.path(), &transcript, &commit, "tests").unwrap();
    }

    #[test]
    fn accepts_successful_pi_split_workspace_and_path_inspection() {
        let (workspace, commit) = committed_workspace();
        let artifact = write_inspection(
            workspace.path(),
            &commit,
            "tests",
            "src/lib.rs",
            "Checked the candidate's test surface.",
        );
        let transcript = workspace.path().join("transcript.jsonl");
        fs::write(
            &transcript,
            format!(
                "{{\"type\":\"tool_execution_start\",\"toolCallId\":\"tool-1\",\"toolName\":\"read\",\"args\":{{\"cwd\":\"{}\",\"path\":\"src/lib.rs\"}}}}\n{{\"type\":\"tool_execution_end\",\"toolCallId\":\"tool-1\",\"toolName\":\"read\",\"result\":{{}},\"isError\":false}}\n",
                workspace.path().display()
            ),
        )
        .unwrap();
        validate(&artifact, workspace.path(), &transcript, &commit, "tests").unwrap();
    }

    #[test]
    fn accepts_successful_native_pi_inspection() {
        let (workspace, commit) = committed_workspace();
        let artifact = write_inspection(
            workspace.path(),
            &commit,
            "tests",
            "src/lib.rs",
            "Checked the candidate's test surface.",
        );
        let inspected = workspace.path().join("src/lib.rs").display().to_string();
        let transcript = workspace.path().join("transcript.jsonl");
        let envelope = |event| {
            serde_json::json!({
                "format": "fluent-native-agent-event",
                "schema_version": 1,
                "provider": "pi",
                "event": event,
            })
            .to_string()
        };
        fs::write(
            &transcript,
            format!(
                "{}\n{}\n",
                envelope(serde_json::json!({"type":"tool_execution_start","toolCallId":"tool-1","toolName":"read","args":{"path":inspected}})),
                envelope(serde_json::json!({"type":"tool_execution_end","toolCallId":"tool-1","toolName":"read","result":{},"isError":false})),
            ),
        )
        .unwrap();
        validate(&artifact, workspace.path(), &transcript, &commit, "tests").unwrap();

        fs::write(
            &transcript,
            format!(
                "{}\n{}\n",
                envelope(serde_json::json!({"type":"tool_execution_start","toolCallId":"tool-1","toolName":"read","args":{"path":inspected}})),
                envelope(serde_json::json!({"type":"tool_execution_end","toolCallId":"tool-1","toolName":"read","result":{},"isError":true})),
            ),
        )
        .unwrap();
        assert!(validate(&artifact, workspace.path(), &transcript, &commit, "tests").is_err());
    }
}
