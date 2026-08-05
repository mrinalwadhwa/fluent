//! Bounded prompt and token diagnostics for autonomous Writer tasks.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

pub(crate) const WRITER_CONTEXT_USAGE_FILE: &str = "writer-context-usage.json";
pub(crate) const REVIEWED_CORRECTION_PROMPT_MAX_BYTES: usize = 32 * 1024;
const EXECUTION_CONTEXT_MAX_BYTES: u64 = 128 * 1024;
const WRITER_TRANSCRIPT_WARN_BYTES: u64 = 1024 * 1024;
const WRITER_INPUT_TOKEN_WARN: u64 = 1_000_000;
const WRITER_UNCACHED_INPUT_TOKEN_WARN: u64 = 256_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum WriterPromptKind {
    Initial,
    PreReviewContinuation,
    ReviewedCorrection,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct WriterContextUsage {
    schema_version: u32,
    prompt_kind: WriterPromptKind,
    prompt_bytes: u64,
    execution_context_bytes: u64,
    transcript_bytes: u64,
    input_tokens: u64,
    cached_input_tokens: u64,
    uncached_input_tokens: u64,
    output_tokens: u64,
    warnings: Vec<String>,
}

pub(crate) fn record_writer_context_launch(
    artifact_dir: &Path,
    prompt_kind: WriterPromptKind,
    prompt: &str,
    execution_context_path: Option<&Path>,
) -> Result<()> {
    if prompt_kind == WriterPromptKind::ReviewedCorrection
        && prompt.len() > REVIEWED_CORRECTION_PROMPT_MAX_BYTES
    {
        bail!(
            "reviewed correction prompt is {} bytes; limit is {} bytes",
            prompt.len(),
            REVIEWED_CORRECTION_PROMPT_MAX_BYTES
        );
    }
    let execution_context_bytes = execution_context_path
        .and_then(|path| fs::metadata(path).ok())
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    if execution_context_bytes > EXECUTION_CONTEXT_MAX_BYTES {
        bail!(
            "Writer execution context is {execution_context_bytes} bytes; limit is {EXECUTION_CONTEXT_MAX_BYTES} bytes"
        );
    }
    fs::create_dir_all(artifact_dir).with_context(|| {
        format!(
            "create Writer artifact directory {}",
            artifact_dir.display()
        )
    })?;
    if prompt_kind == WriterPromptKind::ReviewedCorrection {
        fs::create_dir_all(artifact_dir.join("commands")).with_context(|| {
            format!(
                "create corrective Writer command log directory {}",
                artifact_dir.join("commands").display()
            )
        })?;
    }
    write_usage(
        artifact_dir,
        &WriterContextUsage {
            schema_version: 1,
            prompt_kind,
            prompt_bytes: prompt.len() as u64,
            execution_context_bytes,
            transcript_bytes: 0,
            input_tokens: 0,
            cached_input_tokens: 0,
            uncached_input_tokens: 0,
            output_tokens: 0,
            warnings: Vec::new(),
        },
    )
}

pub(crate) fn finalize_writer_context_usage(artifact_dir: &Path) -> Result<()> {
    let path = artifact_dir.join(WRITER_CONTEXT_USAGE_FILE);
    let mut usage: WriterContextUsage = serde_json::from_str(
        &fs::read_to_string(&path)
            .with_context(|| format!("read Writer context usage {}", path.display()))?,
    )?;
    usage.transcript_bytes = fs::metadata(artifact_dir.join("transcript.jsonl"))
        .map(|metadata| metadata.len())
        .unwrap_or(0);

    let rows = fs::read_to_string(artifact_dir.join("usage.json"))
        .ok()
        .and_then(|source| serde_json::from_str::<Vec<serde_json::Value>>(&source).ok())
        .unwrap_or_default();
    usage.input_tokens = rows
        .iter()
        .filter_map(|row| row["input_tokens"].as_u64())
        .fold(0, u64::saturating_add);
    usage.cached_input_tokens = rows
        .iter()
        .filter_map(|row| row["cached_input_tokens"].as_u64())
        .fold(0, u64::saturating_add);
    usage.uncached_input_tokens = usage.input_tokens.saturating_sub(usage.cached_input_tokens);
    usage.output_tokens = rows
        .iter()
        .filter_map(|row| row["output_tokens"].as_u64())
        .fold(0, u64::saturating_add);
    usage.warnings.clear();
    if usage.input_tokens > WRITER_INPUT_TOKEN_WARN
        && usage.cached_input_tokens > usage.input_tokens / 2
    {
        usage.warnings.push(format!(
            "cached replay input is high: {} of {} tokens",
            usage.cached_input_tokens, usage.input_tokens
        ));
    }
    if usage.uncached_input_tokens > WRITER_UNCACHED_INPUT_TOKEN_WARN {
        usage.warnings.push(format!(
            "uncached Writer input is high: {} tokens",
            usage.uncached_input_tokens
        ));
    }
    if usage.transcript_bytes > WRITER_TRANSCRIPT_WARN_BYTES {
        usage.warnings.push(format!(
            "Writer transcript is large: {} bytes",
            usage.transcript_bytes
        ));
    }
    write_usage(artifact_dir, &usage)
}

fn write_usage(artifact_dir: &Path, usage: &WriterContextUsage) -> Result<()> {
    let path = artifact_dir.join(WRITER_CONTEXT_USAGE_FILE);
    crate::atomic_write::atomic_write(&path, &serde_json::to_vec_pretty(usage)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn reviewed_correction_prompt_budget_is_enforced_before_launch() {
        let tmp = tempfile::TempDir::new().unwrap();
        let oversized = "x".repeat(REVIEWED_CORRECTION_PROMPT_MAX_BYTES + 1);

        let error = record_writer_context_launch(
            tmp.path(),
            WriterPromptKind::ReviewedCorrection,
            &oversized,
            None,
        )
        .unwrap_err();

        assert!(error.to_string().contains("reviewed correction prompt"));
        assert!(!tmp.path().join(WRITER_CONTEXT_USAGE_FILE).exists());
    }

    #[test]
    fn final_metrics_separate_cached_replay_from_uncached_input() {
        let tmp = tempfile::TempDir::new().unwrap();
        let context = tmp.path().join("execution-context.json");
        fs::write(&context, "{}\n").unwrap();
        record_writer_context_launch(
            tmp.path(),
            WriterPromptKind::ReviewedCorrection,
            "bounded prompt",
            Some(&context),
        )
        .unwrap();
        fs::write(tmp.path().join("transcript.jsonl"), vec![b'x'; 1_100_000]).unwrap();
        fs::write(
            tmp.path().join("usage.json"),
            r#"[{"input_tokens":1400000,"cached_input_tokens":1200000,"output_tokens":12000}]"#,
        )
        .unwrap();

        finalize_writer_context_usage(tmp.path()).unwrap();

        let usage: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(tmp.path().join(WRITER_CONTEXT_USAGE_FILE)).unwrap(),
        )
        .unwrap();
        assert_eq!(usage["prompt-kind"], "reviewed-correction");
        assert_eq!(usage["prompt-bytes"], 14);
        assert_eq!(usage["execution-context-bytes"], 3);
        assert_eq!(usage["input-tokens"], 1_400_000);
        assert_eq!(usage["cached-input-tokens"], 1_200_000);
        assert_eq!(usage["uncached-input-tokens"], 200_000);
        assert_eq!(usage["transcript-bytes"], 1_100_000);
        assert!(
            usage["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|warning| warning.as_str().unwrap().contains("cached replay"))
        );
        assert!(
            usage["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|warning| warning.as_str().unwrap().contains("transcript"))
        );
        assert!(tmp.path().join("commands").is_dir());
    }
}
