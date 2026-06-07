use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::thread;

use crate::coder::{Coder, CoderKind, CoderSandbox};
use crate::content::{ContentResolver, prompt_section};
use crate::run::{ReviewScope, project_root_from_run_dir};

/// Reviewer names in execution order.
pub const REVIEWERS: &[&str] = &[
    "documentation",
    "behaviors",
    "architecture",
    "skills",
    "tests",
];

/// Verdict from a single reviewer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Pass,
    Fail,
    Uncertain,
}

impl Verdict {
    pub fn parse(s: &str) -> Self {
        match s.to_lowercase().trim() {
            "pass" => Self::Pass,
            "fail" => Self::Fail,
            _ => Self::Uncertain,
        }
    }

    pub fn is_passing(&self) -> bool {
        matches!(self, Self::Pass)
    }
}

/// Effective review outcome for a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewState {
    pub state: ReviewStateKind,
    pub round: u32,
    pub source: ReviewStateSource,
    pub verdicts: BTreeMap<String, Verdict>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_rounds: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl ReviewState {
    pub fn from_verdicts(round: u32, verdicts: BTreeMap<String, Verdict>) -> Self {
        let state = if verdicts.values().all(Verdict::is_passing) {
            ReviewStateKind::Passed
        } else if verdicts
            .values()
            .any(|verdict| matches!(verdict, Verdict::Fail))
        {
            ReviewStateKind::Failed
        } else {
            ReviewStateKind::Uncertain
        };
        Self {
            state,
            round,
            source: ReviewStateSource::Reviewers,
            verdicts,
            max_rounds: None,
            reason: None,
        }
    }

    pub fn accepted_review_limit(
        round: u32,
        max_rounds: u32,
        verdicts: BTreeMap<String, Verdict>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            state: ReviewStateKind::AcceptedReviewLimit,
            round,
            source: ReviewStateSource::ReviewLimit,
            verdicts,
            max_rounds: Some(max_rounds),
            reason: Some(reason.into()),
        }
    }

    pub fn is_accepted(&self) -> bool {
        matches!(
            self.state,
            ReviewStateKind::Passed | ReviewStateKind::AcceptedReviewLimit
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewStateKind {
    Passed,
    Failed,
    Uncertain,
    AcceptedReviewLimit,
}

impl ReviewStateKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Uncertain => "uncertain",
            Self::AcceptedReviewLimit => "accepted-review-limit",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewStateSource {
    Reviewers,
    ReviewLimit,
}

impl ReviewStateSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Reviewers => "reviewers",
            Self::ReviewLimit => "review-limit",
        }
    }
}

pub fn write_review_state(run_dir: &Path, state: &ReviewState) -> Result<()> {
    let content = serde_json::to_string_pretty(state)?;
    fs::write(run_dir.join("review-state.json"), format!("{content}\n"))?;
    Ok(())
}

pub fn read_review_state(run_dir: &Path) -> Option<Result<ReviewState>> {
    let path = run_dir.join("review-state.json");
    if !path.exists() {
        return None;
    }
    let content = match fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) => return Some(Err(error.into())),
    };
    Some(serde_json::from_str(&content).map_err(Into::into))
}

pub enum ReviewStateRead {
    Missing,
    Present(ReviewState),
    Invalid(String),
}

pub fn review_state_at(run_dir: &Path) -> ReviewStateRead {
    match read_review_state(run_dir) {
        Some(Ok(state)) => ReviewStateRead::Present(state),
        Some(Err(error)) => ReviewStateRead::Invalid(error.to_string()),
        None => ReviewStateRead::Missing,
    }
}

pub fn effective_review_state(primary_dir: &Path, fallback_dir: &Path) -> ReviewStateRead {
    match review_state_at(primary_dir) {
        ReviewStateRead::Missing if primary_dir != fallback_dir => review_state_at(fallback_dir),
        state => state,
    }
}

pub fn current_review_verdicts(run_dir: &Path) -> BTreeMap<String, Verdict> {
    let reviews_dir = run_dir.join("reviews");
    let mut verdicts = BTreeMap::new();
    let Ok(entries) = fs::read_dir(&reviews_dir) else {
        return verdicts;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .and_then(|stem| stem.strip_prefix("review-"))
        else {
            continue;
        };
        if !path
            .file_name()
            .and_then(|file_name| file_name.to_str())
            .map(|file_name| file_name.ends_with(".md"))
            .unwrap_or(false)
        {
            continue;
        }
        let content = fs::read_to_string(&path).unwrap_or_default();
        verdicts.insert(name.to_string(), extract_verdict(&content));
    }
    verdicts
}

/// Run a single reviewer. Returns the verdict.
pub fn run_single_reviewer(
    reviewer_name: &str,
    system_prompt: &str,
    review_prompt: &str,
    run_dir: &Path,
    coder_kind: CoderKind,
) -> Result<Verdict> {
    // Run from the project root
    let project_root = project_root_from_run_dir(run_dir)
        .to_string_lossy()
        .to_string();

    eprintln!("  [{reviewer_name}] starting...");

    let transcript_path = run_dir.join(format!("reviews/transcript-{reviewer_name}.jsonl"));

    let reviewer = coder_kind.boxed(CoderSandbox::None);
    run_single_reviewer_with_coder(
        reviewer_name,
        system_prompt,
        review_prompt,
        run_dir,
        Path::new(&project_root),
        &*reviewer,
        &transcript_path,
    )
}

fn run_single_reviewer_with_coder(
    reviewer_name: &str,
    system_prompt: &str,
    review_prompt: &str,
    run_dir: &Path,
    project_root: &Path,
    reviewer: &dyn Coder,
    transcript_path: &Path,
) -> Result<Verdict> {
    let exit_code = reviewer.run(
        review_prompt,
        system_prompt,
        project_root,
        &[],
        Some(transcript_path),
    );

    match exit_code {
        Ok(code) if code != 0 => {
            let message = format!("Reviewer session exited with code {code}.");
            eprintln!("  [{reviewer_name}] {message} Marking review failed.");
            write_failure_review_artifact(run_dir, reviewer_name, &message)?;
            return Ok(Verdict::Fail);
        }
        Err(e) => {
            let message = format!("Reviewer failed to launch: {e}.");
            eprintln!("  [{reviewer_name}] {message} Marking review failed.");
            write_failure_review_artifact(run_dir, reviewer_name, &message)?;
            return Ok(Verdict::Fail);
        }
        _ => {}
    }

    // Check for review artifact
    let review_file = run_dir.join(format!("reviews/review-{reviewer_name}.md"));
    if !review_file.exists() {
        let message = format!(
            "Reviewer completed without writing {}.",
            review_file.display()
        );
        eprintln!("  [{reviewer_name}] {message} Marking review failed.");
        write_failure_review_artifact(run_dir, reviewer_name, &message)?;
        return Ok(Verdict::Fail);
    }

    let content = fs::read_to_string(&review_file)?;
    let verdict = extract_verdict(&content);
    eprintln!("  [{reviewer_name}] verdict: {}", verdict_str(&verdict));

    Ok(verdict)
}

fn write_failure_review_artifact(run_dir: &Path, reviewer_name: &str, message: &str) -> Result<()> {
    let reviews_dir = run_dir.join("reviews");
    fs::create_dir_all(&reviews_dir)?;
    let mut file = fs::File::create(reviews_dir.join(format!("review-{reviewer_name}.md")))?;
    writeln!(file, "# {reviewer_name} Review")?;
    writeln!(file)?;
    writeln!(file, "Reviewer: review-{reviewer_name}")?;
    writeln!(file, "Verdict: fail")?;
    writeln!(file)?;
    writeln!(file, "## Findings")?;
    writeln!(file)?;
    writeln!(
        file,
        "Reviewer execution failed before producing a usable review."
    )?;
    writeln!(file)?;
    writeln!(file, "### Failure")?;
    writeln!(file)?;
    writeln!(file, "{message}")?;
    Ok(())
}

/// Archive previous round's review artifacts before running a new round.
fn archive_previous_round(run_dir: &Path, review_round: u32) {
    if review_round <= 1 {
        return;
    }
    let prev_round = review_round - 1;
    let archive_dir = run_dir.join(format!("reviews/round-{prev_round}"));
    let reviews_dir = run_dir.join("reviews");

    if fs::create_dir_all(&archive_dir).is_err() {
        return;
    }

    if let Ok(entries) = fs::read_dir(&reviews_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with("review-") && name_str.ends_with(".md") {
                let _ = fs::rename(entry.path(), archive_dir.join(&name));
            } else if name_str.starts_with("transcript-") && name_str.ends_with(".jsonl") {
                let _ = fs::rename(entry.path(), archive_dir.join(&name));
            }
        }
    }
}

/// Run all reviewers (or a filtered set) in parallel.
/// Returns true if all pass, false if any fail.
/// `review_round` tracks how many times reviews have been run (1-based).
pub fn run_reviews(
    run_dir: &Path,
    run_id: &str,
    reviewer_filter: &str,
    review_scope: ReviewScope,
    resolver: &ContentResolver,
    review_round: u32,
    coder_kind: CoderKind,
) -> Result<bool> {
    fs::create_dir_all(run_dir.join("reviews"))?;

    // Archive previous round's reviews if this isn't the first round
    archive_previous_round(run_dir, review_round);

    let scope_detail = fs::read_to_string(run_dir.join("scope")).unwrap_or_default();
    let scope_instruction = if scope_detail.is_empty() {
        String::new()
    } else {
        format!(
            " Focus your review on: {scope_detail}. Read surrounding context as needed, but concentrate your findings on these areas."
        )
    };

    eprintln!(
        "\n  === Review phase (run: {run_id}, scope: {}) ===\n",
        review_scope.as_str()
    );

    let mut handles = Vec::new();

    for &reviewer in REVIEWERS {
        // Apply filter
        if !reviewer_filter.is_empty() && !reviewer_filter.contains(reviewer) {
            continue;
        }

        // Load prompts
        let prompt_key = format!("prompts/review-{reviewer}.md");
        let prompt_content = match resolver.resolve_content(&prompt_key) {
            Some(c) => c,
            None => {
                let message = format!("Reviewer prompt file missing: {prompt_key}.");
                eprintln!("  [{reviewer}] {message} Marking review failed.");
                write_failure_review_artifact(run_dir, reviewer, &message)?;
                handles.push((
                    reviewer.to_string(),
                    std::thread::spawn(|| Ok(Verdict::Fail)),
                ));
                continue;
            }
        };

        let system = prompt_section(&prompt_content, "system").replace("{{RUN_ID}}", run_id);

        let section = review_scope.as_str();
        let prompt = format!(
            "{}{}",
            prompt_section(&prompt_content, section).replace("{{RUN_ID}}", run_id),
            scope_instruction
        );

        let run_dir = run_dir.to_path_buf();
        let reviewer_name = reviewer.to_string();

        let handle_reviewer = reviewer_name.clone();
        handles.push((
            handle_reviewer,
            std::thread::spawn(move || {
                run_single_reviewer(&reviewer_name, &system, &prompt, &run_dir, coder_kind)
            }),
        ));
    }

    let mut verdicts = BTreeMap::new();
    for (reviewer, handle) in handles {
        let verdict = record_reviewer_result(run_dir, &reviewer, handle.join())?;
        verdicts.insert(reviewer, verdict);
    }

    let state = ReviewState::from_verdicts(review_round, verdicts);
    let all_pass = state.is_accepted();
    write_review_state(run_dir, &state)?;

    Ok(all_pass)
}

fn record_reviewer_result(
    run_dir: &Path,
    reviewer_name: &str,
    result: thread::Result<Result<Verdict>>,
) -> Result<Verdict> {
    let verdict = match result {
        Ok(Ok(verdict)) => verdict,
        Ok(Err(e)) => {
            let message = format!("Reviewer returned an error: {e}.");
            eprintln!("  [{reviewer_name}] {message}");
            write_failure_review_artifact(run_dir, reviewer_name, &message)?;
            Verdict::Fail
        }
        Err(_) => {
            let message = "Reviewer thread panicked.".to_string();
            eprintln!("  [{reviewer_name}] {message}");
            write_failure_review_artifact(run_dir, reviewer_name, &message)?;
            Verdict::Fail
        }
    };
    Ok(verdict)
}

/// Extract verdict from review file content.
pub fn extract_verdict(content: &str) -> Verdict {
    for line in content.lines() {
        let lower = line.to_lowercase();
        if lower.starts_with("verdict:") {
            let value = lower
                .strip_prefix("verdict:")
                .unwrap_or("")
                .trim()
                .to_string();
            return Verdict::parse(&value);
        }
    }
    Verdict::Uncertain
}

fn verdict_str(v: &Verdict) -> &'static str {
    match v {
        Verdict::Pass => "pass",
        Verdict::Fail => "fail",
        Verdict::Uncertain => "uncertain",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;
    use std::path::{Path, PathBuf};

    #[test]
    fn test_extract_verdict_pass() {
        assert_eq!(
            extract_verdict("Verdict: pass\n\nLooks good."),
            Verdict::Pass
        );
    }

    #[test]
    fn test_extract_verdict_fail() {
        assert_eq!(
            extract_verdict("Verdict: fail\n\n1. Missing coverage."),
            Verdict::Fail
        );
    }

    #[test]
    fn test_extract_verdict_uncertain() {
        assert_eq!(
            extract_verdict("Verdict: uncertain\n\nNeed more info."),
            Verdict::Uncertain
        );
    }

    #[test]
    fn test_extract_verdict_case_insensitive() {
        assert_eq!(extract_verdict("Verdict: PASS\n\nAll good."), Verdict::Pass);
        assert_eq!(extract_verdict("verdict: Pass\n"), Verdict::Pass);
    }

    #[test]
    fn test_extract_verdict_missing() {
        assert_eq!(
            extract_verdict("No verdict here.\nJust some text."),
            Verdict::Uncertain
        );
    }

    #[test]
    fn test_verdict_is_passing() {
        assert!(Verdict::Pass.is_passing());
        assert!(!Verdict::Fail.is_passing());
        assert!(!Verdict::Uncertain.is_passing());
    }

    #[test]
    fn test_review_state_round_trip() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut verdicts = BTreeMap::new();
        verdicts.insert("architecture".to_string(), Verdict::Pass);
        verdicts.insert("tests".to_string(), Verdict::Pass);
        let state = ReviewState::from_verdicts(2, verdicts);

        write_review_state(tmp.path(), &state).unwrap();

        let parsed = read_review_state(tmp.path()).unwrap().unwrap();
        assert_eq!(parsed.state, ReviewStateKind::Passed);
        assert_eq!(parsed.round, 2);
        assert_eq!(parsed.source, ReviewStateSource::Reviewers);
        assert!(parsed.is_accepted());
    }

    #[test]
    fn test_review_state_aggregates_non_passing_verdicts() {
        let mut verdicts = BTreeMap::new();
        verdicts.insert("architecture".to_string(), Verdict::Pass);
        verdicts.insert("tests".to_string(), Verdict::Uncertain);
        let state = ReviewState::from_verdicts(1, verdicts);
        assert_eq!(state.state, ReviewStateKind::Uncertain);
        assert!(!state.is_accepted());

        let mut verdicts = BTreeMap::new();
        verdicts.insert("architecture".to_string(), Verdict::Fail);
        verdicts.insert("tests".to_string(), Verdict::Uncertain);
        let state = ReviewState::from_verdicts(1, verdicts);
        assert_eq!(state.state, ReviewStateKind::Failed);
        assert!(!state.is_accepted());
    }

    #[test]
    fn test_review_state_accepts_review_limit() {
        let mut verdicts = BTreeMap::new();
        verdicts.insert("tests".to_string(), Verdict::Fail);

        let state = ReviewState::accepted_review_limit(
            11,
            10,
            verdicts,
            "Review round limit reached with a clean worktree.",
        );

        assert_eq!(state.state, ReviewStateKind::AcceptedReviewLimit);
        assert_eq!(state.source, ReviewStateSource::ReviewLimit);
        assert_eq!(state.max_rounds, Some(10));
        assert!(state.is_accepted());
    }

    #[test]
    fn test_read_review_state_rejects_malformed_json() {
        let tmp = tempfile::TempDir::new().unwrap();
        fs::write(
            tmp.path().join("review-state.json"),
            "{\"state\":\"unknown\"}",
        )
        .unwrap();

        assert!(read_review_state(tmp.path()).unwrap().is_err());
    }

    #[test]
    fn test_archive_previous_round_noop_for_first_round() {
        let tmp = tempfile::TempDir::new().unwrap();
        let run_dir = tmp.path();
        let reviews = run_dir.join("reviews");
        fs::create_dir_all(&reviews).unwrap();
        fs::write(reviews.join("review-tests.md"), "Verdict: pass").unwrap();

        archive_previous_round(run_dir, 1);

        // No archive should be created for round 1
        assert!(!run_dir.join("reviews/round-0").exists());
        // Original file still exists
        assert!(reviews.join("review-tests.md").exists());
    }

    #[test]
    fn test_archive_previous_round_moves_reviews() {
        let tmp = tempfile::TempDir::new().unwrap();
        let run_dir = tmp.path();
        let reviews = run_dir.join("reviews");
        fs::create_dir_all(&reviews).unwrap();
        fs::write(reviews.join("review-tests.md"), "Verdict: pass").unwrap();
        fs::write(
            reviews.join("transcript-tests.jsonl"),
            "{\"type\":\"test\"}",
        )
        .unwrap();

        archive_previous_round(run_dir, 2);

        let archive = reviews.join("round-1");
        assert!(archive.join("review-tests.md").exists());
        assert!(archive.join("transcript-tests.jsonl").exists());

        assert!(!reviews.join("review-tests.md").exists());
        assert!(!reviews.join("transcript-tests.jsonl").exists());
    }

    fn make_run_dir(tmp: &tempfile::TempDir) -> (PathBuf, PathBuf) {
        let project = tmp.path().join("project");
        let run_dir = project.join(".factory/runs/test-run");
        fs::create_dir_all(run_dir.join("reviews")).unwrap();
        (project, run_dir)
    }

    struct TestReviewer {
        exit: Result<i32>,
        write_review: bool,
    }

    impl Coder for TestReviewer {
        fn run(
            &self,
            _prompt: &str,
            _system_prompt: &str,
            working_dir: &Path,
            _extra_args: &[String],
            _transcript_file: Option<&Path>,
        ) -> Result<i32> {
            if self.write_review {
                fs::write(
                    working_dir.join(".factory/runs/test-run/reviews/review-tests.md"),
                    "Verdict: pass\n\nLooks good.\n",
                )?;
            }
            match &self.exit {
                Ok(code) => Ok(*code),
                Err(e) => Err(anyhow!("{e}")),
            }
        }

        fn run_interactive(
            &self,
            _system_prompt: &str,
            _working_dir: &Path,
            _extra_args: &[String],
        ) -> Result<i32> {
            Ok(0)
        }
    }

    #[test]
    fn test_run_single_reviewer_fails_on_nonzero_exit() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (project, run_dir) = make_run_dir(&tmp);
        let reviewer = TestReviewer {
            exit: Ok(12),
            write_review: false,
        };

        let verdict = run_single_reviewer_with_coder(
            "tests",
            "system",
            "prompt",
            &run_dir,
            &project,
            &reviewer,
            &run_dir.join("reviews/transcript-tests.jsonl"),
        )
        .unwrap();

        assert_eq!(verdict, Verdict::Fail);
        let review = fs::read_to_string(run_dir.join("reviews/review-tests.md")).unwrap();
        assert!(review.contains("Verdict: fail"));
        assert!(review.contains("exited with code 12"));
    }

    #[test]
    fn test_run_single_reviewer_fails_on_launch_error() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (project, run_dir) = make_run_dir(&tmp);
        let reviewer = TestReviewer {
            exit: Err(anyhow!("missing reviewer command")),
            write_review: false,
        };

        let verdict = run_single_reviewer_with_coder(
            "tests",
            "system",
            "prompt",
            &run_dir,
            &project,
            &reviewer,
            &run_dir.join("reviews/transcript-tests.jsonl"),
        )
        .unwrap();

        assert_eq!(verdict, Verdict::Fail);
        let review = fs::read_to_string(run_dir.join("reviews/review-tests.md")).unwrap();
        assert!(review.contains("Verdict: fail"));
        assert!(review.contains("missing reviewer command"));
    }

    #[test]
    fn test_run_single_reviewer_fails_without_review_artifact() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (project, run_dir) = make_run_dir(&tmp);
        let reviewer = TestReviewer {
            exit: Ok(0),
            write_review: false,
        };

        let verdict = run_single_reviewer_with_coder(
            "tests",
            "system",
            "prompt",
            &run_dir,
            &project,
            &reviewer,
            &run_dir.join("reviews/transcript-tests.jsonl"),
        )
        .unwrap();

        assert_eq!(verdict, Verdict::Fail);
        let review = fs::read_to_string(run_dir.join("reviews/review-tests.md")).unwrap();
        assert!(review.contains("Verdict: fail"));
        assert!(review.contains("without writing"));
    }

    #[test]
    fn test_run_single_reviewer_passes_with_pass_artifact() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (project, run_dir) = make_run_dir(&tmp);
        let reviewer = TestReviewer {
            exit: Ok(0),
            write_review: true,
        };

        let verdict = run_single_reviewer_with_coder(
            "tests",
            "system",
            "prompt",
            &run_dir,
            &project,
            &reviewer,
            &run_dir.join("reviews/transcript-tests.jsonl"),
        )
        .unwrap();

        assert_eq!(verdict, Verdict::Pass);
    }

    #[test]
    fn test_run_reviews_fails_when_reviewer_prompt_is_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (project, run_dir) = make_run_dir(&tmp);
        fs::create_dir_all(project.join(".factory/prompts/review-tests.md")).unwrap();
        let resolver = ContentResolver::new(Some(&project));

        let all_pass = run_reviews(
            &run_dir,
            "test-run",
            "tests",
            ReviewScope::Changes,
            &resolver,
            1,
            CoderKind::Claude,
        )
        .unwrap();

        assert!(!all_pass);
        let review = fs::read_to_string(run_dir.join("reviews/review-tests.md")).unwrap();
        assert!(review.contains("Reviewer: review-tests"));
        assert!(review.contains("Verdict: fail"));
        assert!(review.contains("Reviewer prompt file missing: prompts/review-tests.md."));
    }

    #[test]
    fn test_reviewer_result_errors_and_panics_are_not_passing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (_project, run_dir) = make_run_dir(&tmp);

        let verdict = record_reviewer_result(&run_dir, "tests", Ok(Ok(Verdict::Pass))).unwrap();
        assert_eq!(verdict, Verdict::Pass);

        let verdict =
            record_reviewer_result(&run_dir, "tests", Ok(Err(anyhow!("reviewer failed")))).unwrap();
        assert_eq!(verdict, Verdict::Fail);
        let review = fs::read_to_string(run_dir.join("reviews/review-tests.md")).unwrap();
        assert!(review.contains("Verdict: fail"));
        assert!(review.contains("reviewer failed"));

        let verdict = record_reviewer_result(
            &run_dir,
            "tests",
            Err(Box::new("panic") as Box<dyn std::any::Any + Send>),
        )
        .unwrap();
        assert_eq!(verdict, Verdict::Fail);
        let review = fs::read_to_string(run_dir.join("reviews/review-tests.md")).unwrap();
        assert!(review.contains("Reviewer thread panicked"));
    }
}
