//! Public API surface tests. These compile only against `fluent`'s public API, so
//! they prove an external caller can use a capability without reaching into private
//! internals.

use std::path::Path;

use fluent::coder::{Coder, TranscriptCapture};

/// A minimal external `Coder` implementation, standing in for a caller outside the
/// built-in coders. It records the transcript path it was handed through the public
/// capture boundary.
struct ExternalCoder;

impl Coder for ExternalCoder {
    fn run(
        &self,
        _prompt: &str,
        _system_prompt: &str,
        _working_dir: &Path,
        _extra_args: &[String],
        _extra_env: &[(String, String)],
        transcript_file: Option<&Path>,
    ) -> anyhow::Result<i32> {
        // Prove the capture's transcript path reached this external implementation.
        assert!(transcript_file.is_some());
        Ok(0)
    }

    fn run_interactive(
        &self,
        _system_prompt: &str,
        _working_dir: &Path,
        _extra_args: &[String],
        _extra_env: &[(String, String)],
    ) -> anyhow::Result<i32> {
        Ok(0)
    }
}

#[test]
fn external_coder_can_construct_transcript_capture() {
    // The public constructor accepts a transcript path and a project root and
    // resolves this project's pump thresholds internally — the caller never names
    // the private pump configuration type.
    let dir = tempfile::tempdir().unwrap();
    let transcript = dir.path().join("transcript.jsonl");

    let capture = TranscriptCapture::new(&transcript, dir.path());
    assert_eq!(
        capture.path(),
        transcript.as_path(),
        "the public path accessor returns the capture's transcript path"
    );

    // An external coder can thread the capture through the public `run_captured`
    // boundary (its default implementation forwards the capture's path to `run`).
    let coder = ExternalCoder;
    let exit = coder
        .run_captured("prompt", "system", dir.path(), &[], &[], Some(&capture))
        .unwrap();
    assert_eq!(exit, 0);
}
