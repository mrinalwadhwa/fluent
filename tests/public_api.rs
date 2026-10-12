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
fn learner_run_inputs_handoff_only_surface_remains_source_compatible() {
    // An external caller still constructs `LearnerRunInputs` with the Boolean
    // `handoff_only` surface: `false` is capture, `true` is post-land handoff-only.
    // No public no-expertise switch is exposed, so a caller cannot request the
    // crate-private pre-land no-expertise mode.
    use fluent::coder::CoderKind;
    use fluent::content::ContentResolver;
    use fluent::work_task_executor::LearnerRunInputs;
    use std::path::PathBuf;

    let resolver = ContentResolver::new(None);
    let workspace = PathBuf::from("/tmp/ws");
    let handoff = PathBuf::from("/tmp/handoff");
    let make = |handoff_only: bool| LearnerRunInputs {
        native_identity: None,
        workspace_path: &workspace,
        resolver: &resolver,
        extra_args: &[],
        coder_kind: CoderKind::Claude,
        no_sandbox: false,
        model: None,
        effort: None,
        review_artifact_paths: &[],
        tester_artifact_paths: &[],
        diff_command: "git diff",
        handoff_dir: &handoff,
        denied_write_roots: &[],
        handoff_only,
        repair: None,
    };
    assert!(!make(false).handoff_only, "false is the capture surface");
    assert!(
        make(true).handoff_only,
        "true is the post-land handoff-only surface"
    );

    // The public Boolean-only entry point still resolves.
    let _run = fluent::work_task_executor::run_learner;
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
